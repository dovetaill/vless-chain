"""Migration checks use fixtures and mocked service managers only."""
import contextlib
import grp
import io
import json
import os
from pathlib import Path
import pwd
import shutil
import sqlite3
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import Mock, patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import chain_legacy as legacy
import chain_install as installer
import chain_network as network


class Legacy(unittest.TestCase):
    def test_xui_database_keeps_disabled_connection_settings_and_excludes_remote_nodes(self):
        with contextlib.closing(sqlite3.connect(':memory:')) as connection:
            connection.execute('CREATE TABLE inbounds(port INTEGER, protocol TEXT, enable INTEGER, remark TEXT, settings TEXT, stream_settings TEXT, sniffing TEXT, listen TEXT, tag TEXT, node_id INTEGER)')
            for enabled, node in [(0, None), (1, 0), (1, 7)]:
                connection.execute('INSERT INTO inbounds VALUES(?,?,?,?,?,?,?,?,?,?)',
                    (443, 'vless', enabled, 'old', '{"clients":[{"id":"keep-uuid"}]}', '{"network":"ws"}', 'null', '', 'inbound', node))
            values = legacy.xui_inbounds(connection)
            self.assertEqual(len(values), 2)
            self.assertFalse(values[0]['enable'])
            self.assertTrue(values[1]['enable'])
            self.assertEqual(values[0]['settings']['clients'][0]['id'], 'keep-uuid')
            self.assertEqual(values[0]['streamSettings']['network'], 'ws')

    def test_non_xray_panel_inbound_blocks_automatic_handover(self):
        with tempfile.TemporaryDirectory() as temporary:
            proc = Path(temporary)
            process = proc / '20'
            (process / 'ns').mkdir(parents=True)
            (process / 'ns/pid').symlink_to('pid:[1]')
            (process / 'exe').symlink_to('/usr/bin/xray')
            (process / 'cwd').symlink_to('/')
            (process / 'cmdline').write_bytes(b'xray\0-config\0/config.json\0')
            (process / 'cgroup').write_text('0::/\n')
            db = process / 'root/etc/x-ui/x-ui.db'
            db.parent.mkdir(parents=True)
            with contextlib.closing(sqlite3.connect(db)) as connection:
                connection.execute('CREATE TABLE inbounds(port INTEGER, protocol TEXT, enable INTEGER, remark TEXT, settings TEXT, stream_settings TEXT, sniffing TEXT, listen TEXT, tag TEXT)')
                connection.execute("INSERT INTO inbounds VALUES(8443,'tuic',1,'native','{}','{}','null','','native')")
                connection.commit()
            (process / 'root/config.json').write_text(json.dumps({'inbounds': [{'port': 443, 'protocol': 'vless'}]}))
            with patch.object(legacy.shutil, 'which', return_value=None), patch.object(legacy, 'run') as run:
                source = legacy.discover(proc)[0]['sources'][0]
            run.assert_not_called()
            self.assertIn('不能自动停用', source['error'])
            self.assertEqual(source['source_inbounds'][0]['protocol'], 'tuic')

    def test_geodata_is_copied_without_changing_original_files(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            assets = root / 'old/usr/bin'
            assets.mkdir(parents=True)
            original = assets / 'geoip.dat'
            original.write_bytes(b'fixture-geodata')
            before = original.stat()
            owner = {'files': {}, 'sources': []}
            config = {'routing': {'rules': [{'ip': ['geoip:private', 'geoip:cn']}]}}
            files = legacy.collect_assets(config, root / 'old', Path('/'), Path('/usr/bin'), owner)
            source = {'error': '', 'asset_files': files}
            owner['sources'].append(source)
            manager = installer.Manager(root / 'new')
            legacy.stage_assets(manager, [owner])
            saved = Path(source['asset_dir']) / 'geoip.dat'
            self.assertEqual(saved.read_bytes(), original.read_bytes())
            self.assertEqual(saved.stat().st_mode & 0o777, 0o644)
            legacy.stage_assets(manager, [owner])
            after = original.stat()
            self.assertEqual((before.st_uid, before.st_gid, before.st_mode, before.st_ctime_ns),
                             (after.st_uid, after.st_gid, after.st_mode, after.st_ctime_ns))
            with self.assertRaisesRegex(ValueError, '文件名无效'):
                legacy.collect_assets({'routing': {'rules': [{'domain': ['ext:../escape:all']}]}},
                                      root / 'old', Path('/'), Path('/usr/bin'), owner)

    def test_outbound_client_certificate_is_inlined(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / 'cert.pem').write_text('client certificate\n')
            config = {'inbounds': [], 'outbounds': [{'streamSettings': {'tlsSettings': {
                'certificates': [{'certificateFile': '/cert.pem'}]}}}]}
            owner = {'files': {}}
            legacy.hydrate(config, root, Path('/'), owner)
            self.assertEqual(config['outbounds'][0]['streamSettings']['tlsSettings']['certificates'],
                             [{'certificate': ['client certificate']}])

    def test_hydrate_nullable_tls_and_preserves_certificate_loading(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            cert = root / 'cert.pem'
            cert.write_text('certificate\nsecond line\n')
            config = {'inbounds': [{}, {'streamSettings': None},
                {'streamSettings': {'tlsSettings': None}},
                {'streamSettings': {'tlsSettings': {'certificates': None}}},
                {'streamSettings': {'tlsSettings': {'certificates': [
                    {'certificateFile': 'cert.pem'}, {'certificate': ['inline']}]}}}]}
            owner = {'files': {}}
            legacy.hydrate(config, root, Path('/'), owner)
            certs = config['inbounds'][-1]['streamSettings']['tlsSettings']['certificates']
            self.assertEqual(certs, [{'certificate': ['certificate', 'second line']}, {'certificate': ['inline']}])
            self.assertEqual(owner['files']['/cert.pem']['body'], cert.read_bytes())

    def test_invalid_inbounds_with_xui_database_do_not_abort_discovery(self):
        for inbounds in [None, {}, 'invalid', [None], ['invalid'], [{'port': []}]]:
            with self.subTest(inbounds=inbounds), tempfile.TemporaryDirectory() as temporary:
                proc = Path(temporary)
                process = proc / '20'
                (process / 'ns').mkdir(parents=True)
                (process / 'ns/pid').symlink_to('pid:[1]')
                (process / 'exe').symlink_to('/usr/bin/xray')
                (process / 'cwd').symlink_to('/')
                (process / 'cmdline').write_bytes(b'xray\0-config\0/config.json\0')
                (process / 'cgroup').write_text('0::/\n')
                db = process / 'root/etc/x-ui/x-ui.db'
                db.parent.mkdir(parents=True)
                with contextlib.closing(sqlite3.connect(db)) as connection:
                    connection.execute('CREATE TABLE inbounds(port INTEGER, remark TEXT)')
                    connection.execute("INSERT INTO inbounds VALUES(443, 'existing proxy')")
                    connection.commit()
                config = process / 'root/config.json'
                config.write_text(json.dumps({'inbounds': inbounds}))
                before = {p: p.stat() for p in [db, config]}
                with patch.object(legacy.shutil, 'which', return_value=None), \
                        patch.object(legacy, 'run') as run, patch.object(legacy, 'output') as output:
                    owners = legacy.discover(proc)
                run.assert_not_called()
                output.assert_not_called()
                self.assertEqual(len(owners), 1)
                self.assertEqual(bool(owners[0]['sources'][0]['error']), inbounds != [{'port': []}])
                for path, old in before.items():
                    new = path.stat()
                    self.assertEqual((old.st_uid, old.st_gid, old.st_mode, old.st_ctime_ns),
                                     (new.st_uid, new.st_gid, new.st_mode, new.st_ctime_ns))

    def test_invalid_certificate_paths_preserve_original_reference(self):
        for value in [None, '', [], 'missing.pem']:
            with self.subTest(value=value), tempfile.TemporaryDirectory() as temporary:
                cert = {'certificateFile': value}
                config = {'inbounds': [{'streamSettings': {'tlsSettings': {'certificates': [cert]}}}]}
                with self.assertRaises((ValueError, OSError)):
                    legacy.hydrate(config, Path(temporary), Path('/'), {'files': {}})
                self.assertEqual(cert, {'certificateFile': value})

    def test_missing_container_mapping_is_a_configuration_error(self):
        for network in [None, {}, {'Ports': None}, {'Ports': {'443/tcp': 'invalid'}},
                        {'Ports': {'443/tcp': [None]}}]:
            with self.subTest(network=network), self.assertRaises(ValueError):
                legacy.published_ports({'inbounds': [{'protocol': 'vless', 'port': 443}]},
                                       {'NetworkSettings': network})

    def test_discovery_null_stream_does_not_abort_installation(self):
        variants = [None, {'tlsSettings': None}, {'tlsSettings': {'certificates': None}},
                    'invalid', {'tlsSettings': 'invalid'}, {'tlsSettings': {'certificates': {}}},
                    {'tlsSettings': {'certificates': [None]}}]
        for index, stream in enumerate(variants):
            with self.subTest(stream=stream), tempfile.TemporaryDirectory() as temporary:
                proc = Path(temporary)
                process = proc / '20'
                (process / 'ns').mkdir(parents=True)
                (process / 'ns/pid').symlink_to('pid:[1]')
                (process / 'exe').symlink_to('/usr/bin/xray')
                (process / 'cwd').symlink_to('/')
                (process / 'cmdline').write_bytes(b'xray\0-config\0/config.json\0')
                (process / 'cgroup').write_text('0::/\n')
                (process / 'root').mkdir()
                config = process / 'root/config.json'
                config.write_text(json.dumps({'inbounds': [{'streamSettings': stream}]}))
                before = config.stat()
                with patch.object(legacy.shutil, 'which', return_value=None), \
                        patch.object(legacy, 'run') as run, patch.object(legacy, 'output') as output:
                    owners = legacy.discover(proc)
                run.assert_not_called()
                output.assert_not_called()
                self.assertEqual(len(owners), 1)
                source = owners[0]['sources'][0]
                self.assertEqual(bool(source['error']), index >= 3)
                self.assertEqual(source['config']['inbounds'][0]['streamSettings'], stream)
                after = config.stat()
                self.assertEqual((before.st_uid, before.st_gid, before.st_mode, before.st_ctime_ns),
                                 (after.st_uid, after.st_gid, after.st_mode, after.st_ctime_ns))

    def test_acme_directory_preserves_existing_metadata_and_rejects_links(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary) / 'acme/panel'
            network.prepare_proxy_acme_directory(root, os.geteuid(), os.getegid())
            before = {p: p.stat() for p in [root.parent, root, *root.rglob('*')]}
            network.prepare_proxy_acme_directory(root, os.geteuid(), os.getegid())
            for path, old in before.items():
                current = path.stat()
                self.assertEqual((old.st_uid, old.st_gid, old.st_mode, old.st_ctime_ns),
                                 (current.st_uid, current.st_gid, current.st_mode, current.st_ctime_ns))
            link = root.parent / 'agent'
            link.symlink_to(root, target_is_directory=True)
            with self.assertRaisesRegex(ValueError, '归属不匹配'):
                network.prepare_proxy_acme_directory(link, os.geteuid(), os.getegid())

    def test_json_comments_preserve_urls_and_strings(self):
        value = legacy.json_config('/* comment */ {"url":"https://example.com//x", // comment\n"key":"a\\\"//b"}')
        self.assertEqual(value, {'url': 'https://example.com//x', 'key': 'a"//b'})

    def test_config_flags_use_process_working_directory(self):
        self.assertEqual(legacy.configs(['xray', 'run', '-config=bin/config.json'], Path('/opt/x-ui')),
                         [(Path('/opt/x-ui/bin/config.json'), False)])
        self.assertEqual(legacy.configs(['xray', '-confdir', '/etc/xray'], Path('/')),
                         [(Path('/etc/xray'), True)])

    def test_container_port_mapping_preserves_public_port(self):
        config = {'inbounds': [{'protocol': 'vmess', 'port': 443}]}
        container = {'NetworkSettings': {'Ports': {'443/tcp': [{'HostPort': '8443', 'HostIp': '0.0.0.0'}]}}}
        legacy.published_ports(config, container)
        self.assertEqual(config['inbounds'][0]['port'], 8443)
        container['NetworkSettings']['Ports']['443/tcp'][0]['HostIp'] = '127.0.0.1'
        config['inbounds'][0]['port'] = 443
        with self.assertRaises(ValueError):
            legacy.published_ports(config, container)

    def test_failed_stop_restores_only_selected_services_and_restart_policy(self):
        with tempfile.TemporaryDirectory() as temporary:
            owners = [{'id': 'x-ui.service', 'kind': 'systemd', 'name': 'x-ui', 'files': {}, 'enabled': True, 'active': True},
                      {'id': 'a' * 64, 'kind': 'docker', 'name': 'xray', 'files': {},
                       'container': {'HostConfig': {'RestartPolicy': {'Name': 'on-failure', 'MaximumRetryCount': 4}}}}]
            handover = legacy.Handover(owners, temporary)
            calls = []
            def run(command, **_):
                calls.append(command)
                if command[:2] == ['docker', 'stop']:
                    raise subprocess.CalledProcessError(1, command)
                return Mock(returncode=3 if 'is-active' in command else 0)
            with patch.object(legacy, 'run', side_effect=run):
                with self.assertRaises(subprocess.CalledProcessError):
                    handover.stop()
                handover.restore()
            self.assertIn(['docker', 'update', '--restart=on-failure:4', 'a' * 64], calls)
            self.assertIn(['systemctl', 'start', 'x-ui.service'], calls)
            self.assertFalse(any('nginx' in str(call) for call in calls))

    def test_unmanaged_proxy_cannot_be_stopped_by_menu(self):
        choose = Mock()
        owners = [{'kind': 'unmanaged', 'id': '2', 'name': 'xray', 'pids': [2]}]
        self.assertEqual(legacy.select(owners, choose), [])
        choose.assert_not_called()

    def test_proxy_acme_ignores_443_and_preserves_foreign_80(self):
        with patch.object(network, 'foreign_listeners', return_value='xray') as probe, \
                patch.object(network, 'HttpsSite') as site:
            self.assertFalse(network.install_proxy_acme('agent', 1000, 1000))
            probe.assert_called_once_with((80,))
            site.return_value.prepare.assert_not_called()

    def test_custom_https_port_is_used_in_listener_and_redirect(self):
        site = network.HttpsSite('panel.example.com', 12345, True)
        site.https_port = 24443
        config = site.config(tls=True)
        self.assertIn('listen 24443 ssl;', config)
        self.assertIn('listen [::]:24443 ssl;', config)
        self.assertIn('https://panel.example.com:24443$request_uri', config)
        self.assertNotIn('listen 443 ', config)

    def test_discovery_never_adopts_sshd_as_a_proxy_service(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            for pid, executable in [('1', 'init'), ('10', 'sshd'), ('20', 'xray')]:
                process = root / pid
                (process / 'ns').mkdir(parents=True)
                (process / 'ns/pid').symlink_to('pid:[1]')
                (process / 'exe').symlink_to('/usr/bin/' + executable)
                (process / 'cwd').symlink_to('/')
                (process / 'cmdline').write_bytes(executable.encode() + b'\0')
                (process / 'cgroup').write_text('0::/system.slice/sshd.service\n')
            with patch.object(legacy.shutil, 'which', return_value=None), patch.object(legacy, 'output', return_value='10'):
                owners = legacy.discover(root)
            self.assertEqual(len(owners), 1)
            self.assertEqual(owners[0]['kind'], 'unmanaged')

    def test_openrc_discovery_requires_matching_host_supervisor(self):
        for variant in ['managed', 'wrong-parent', 'wrong-service', 'missing-unit', 'container', 'platform-service']:
            with self.subTest(variant=variant), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                proc, system = root / 'proc', root / 'system'
                name = 'chain-agent' if variant == 'platform-service' else 'xray-chain-example'
                unit = system / 'etc/init.d' / name
                unit.parent.mkdir(parents=True)
                if variant != 'missing-unit':
                    unit.write_text('#!/sbin/openrc-run\ncommand=/usr/local/bin/xray\n')
                config = system / 'etc/conf.d' / name
                config.parent.mkdir(parents=True)
                config.write_text('respawn_delay=3\n')
                for pid, executable in [('1', '/sbin/init'), ('10', '/sbin/supervise-daemon'), ('20', '/usr/local/bin/xray')]:
                    process = proc / pid
                    (process / 'ns').mkdir(parents=True)
                    namespace = 'pid:[2]' if pid == '20' and variant == 'container' else 'pid:[1]'
                    (process / 'ns/pid').symlink_to(namespace)
                    (process / 'exe').symlink_to(Path('/usr/sbin/sshd' if pid == '10' and variant == 'wrong-parent' else executable).resolve())
                    (process / 'cwd').symlink_to('/')
                    (process / 'root').symlink_to(system)
                    (process / 'cmdline').write_bytes(executable.encode() + b'\0')
                    (process / 'cgroup').write_text('0::/\n')
                    (process / 'status').write_text('PPid:\t10\n')
                service = 'sshd' if variant == 'wrong-service' else name
                (proc / '10/cmdline').write_bytes(f'supervise-daemon\0{name}\0--start\0/usr/local/bin/xray\0'.encode())
                (proc / '10/environ').write_bytes(f'RC_SVCNAME={name}\0RC_SERVICE=/etc/init.d/{service}\0'.encode())
                with patch.object(legacy.shutil, 'which', side_effect=lambda name: '/sbin/supervise-daemon' if name == 'supervise-daemon' else None), \
                        patch.object(legacy, 'run') as run:
                    owners = legacy.discover(proc)
                run.assert_not_called()
                self.assertEqual(len(owners), 1)
                owner = owners[0]
                self.assertEqual(owner['kind'], 'openrc' if variant == 'managed' else 'unmanaged')
                self.assertEqual(owner['pids'], [20])
                if variant == 'managed':
                    self.assertEqual(owner['id'], name)
                    self.assertEqual(owner['files']['/etc/init.d/' + name]['body'], unit.read_bytes())
                    self.assertEqual(owner['files']['/etc/conf.d/' + name]['body'], config.read_bytes())

    def test_openrc_handover_restores_original_runlevels_on_failure(self):
        for variant in ['success', 'stop-fails', 'status-fails', 'inactive', 'unit-changed']:
            with self.subTest(variant=variant), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                name = 'xray-chain-example'
                unit = root / name
                body = b'#!/sbin/openrc-run\ncommand=/usr/local/bin/xray\n'
                unit.write_bytes(body)
                levels = root / 'runlevels'
                for level in ['default', 'custom']:
                    (levels / level).mkdir(parents=True)
                    (levels / level / name).symlink_to(unit)
                owner = {'id': name, 'name': name, 'kind': 'openrc', 'sources': [], 'files': {
                    '/etc/init.d/' + name: {'path': str(unit), 'body': body}}}
                handover = legacy.Handover([owner], root / 'backups')
                calls = []
                def run(command, **_):
                    calls.append(command)
                    if command == ['rc-service', name, 'stop'] and variant == 'stop-fails':
                        raise subprocess.CalledProcessError(1, command)
                    code = 0
                    if command == ['rc-service', name, 'status']:
                        code = 3 if variant == 'inactive' or ['rc-service', name, 'stop'] in calls else 0
                        if variant == 'status-fails' and ['rc-service', name, 'stop'] in calls:
                            code = 1
                    return Mock(returncode=code)
                with patch.object(legacy, 'Path', side_effect=lambda p: levels if p == '/etc/runlevels' else Path(p)), \
                        patch.object(legacy, 'run', side_effect=run):
                    handover.prepare()
                    metadata = json.loads((handover.backup / '0/source.json').read_text())
                    self.assertEqual(metadata['runlevels'], ['custom', 'default'])
                    self.assertEqual(metadata['active'], variant != 'inactive')
                    self.assertEqual((handover.backup / '0/file-0').read_bytes(), body)
                    if variant == 'unit-changed':
                        unit.write_bytes(body + b'# concurrent change\n')
                        with self.assertRaisesRegex(ValueError, '旧配置在安装期间已变化'):
                            handover.stop()
                        self.assertEqual(handover.changed, [])
                        self.assertEqual(calls, [['rc-service', name, 'status']])
                        continue
                    if variant == 'stop-fails':
                        with self.assertRaises(subprocess.CalledProcessError):
                            handover.stop()
                    elif variant == 'status-fails':
                        with self.assertRaisesRegex(ValueError, '未确认停止'):
                            handover.stop()
                    else:
                        handover.stop()
                    handover.restore()
                for level in ['custom', 'default']:
                    self.assertIn(['rc-update', 'del', name, level], calls)
                    self.assertIn(['rc-update', 'add', name, level], calls)
                self.assertEqual(['rc-service', name, 'start'] in calls, variant != 'inactive')
                self.assertTrue(all(c[0] in ['rc-service', 'rc-update'] and name in c for c in calls))
                self.assertEqual(unit.read_bytes(), body)

    def test_backup_is_private_and_never_rewrites_source_config(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            config = root / 'config.json'
            body = b'{"inbounds":[]}'
            config.write_bytes(body)
            owner = {'id': 'xray.service', 'name': 'xray', 'kind': 'systemd', 'sources': [], 'files': {
                '/etc/xray/config.json': {'path': str(config), 'body': body}}}
            handover = legacy.Handover([owner], root / 'backups')
            with patch.object(legacy, 'output', side_effect=['[Service]\nExecStart=/usr/bin/xray', 'enabled']), \
                    patch.object(legacy, 'run', return_value=Mock(returncode=0)):
                handover.prepare()
            self.assertEqual(config.read_bytes(), body)
            self.assertEqual(handover.backup.stat().st_mode & 0o777, 0o700)
            for path in handover.backup.rglob('*'):
                if path.is_file():
                    self.assertEqual(path.stat().st_mode & 0o777, 0o600)

    @unittest.skipUnless(shutil.which('nginx') and shutil.which('openssl'), 'Nginx / OpenSSL unavailable')
    def test_manual_https_config_and_failure_rollback_leave_other_sites_untouched(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            cert, key = root / 'fullchain.pem', root / 'privkey.pem'
            subprocess.run(['openssl', 'req', '-x509', '-newkey', 'ec', '-pkeyopt', 'ec_paramgen_curve:prime256v1',
                            '-nodes', '-days', '1', '-subj', '/CN=panel.example.com', '-keyout', key, '-out', cert],
                           check=True, capture_output=True)
            site = network.HttpsSite('panel.example.com', 12345)
            site.certificate, site.key, site.https_port = cert, key, 24443
            config, unit = root / 'https/nginx.conf', root / 'https.service'
            foreign = root / 'other.conf'
            foreign.write_text('preserve this site')
            def command(argv, **kwargs):
                if '-t' in argv:
                    text = config.read_text().replace('/var/run/chain-panel-https.pid', str(root / 'nginx.pid')).replace('/var/log/chain-panel-https.log', str(root / 'nginx.log'))
                    for name in ('client_body', 'proxy', 'fastcgi', 'uwsgi', 'scgi'):
                        self.assertIn(f'{name}_temp_path "{root}/temporary/{name}";', text)
                    worker = pwd.getpwnam('nobody')
                    self.assertIn(f'user "{worker.pw_name}" "{grp.getgrgid(worker.pw_gid).gr_name}";', text)
                    self.assertIn(f'lock_file "{root}/temporary/nginx.lock";', text)
                    self.assertNotIn('/var/lib/nginx', text)
                    validation = root / 'validate.conf'
                    validation.write_text(text)
                    command = ['nginx', '-t', '-p', root, '-c', validation]
                    if '-e filename' in subprocess.run(['nginx', '-h'], capture_output=True, text=True).stderr:
                        command += ['-e', root / 'validation.log']
                    result = subprocess.run(command, capture_output=True, text=True)
                    self.assertEqual(result.returncode, 0, result.stderr)
                    return result
                return Mock(returncode=0)
            paths = {'/etc/chain-panel-https': root / 'https', '/var/lib/chain-panel-https': root / 'temporary'}
            with patch.object(network, 'Path', side_effect=lambda p: paths.get(p, Path(p))), \
                    patch.object(network, 'service_path', return_value=unit), patch.object(network, 'service_kind', return_value='systemd'), \
                    patch.object(network, 'listeners', return_value=''), patch.object(network, 'install_packages'), \
                    patch.object(network, 'run', side_effect=command), patch.object(network, 'service') as service, \
                    patch.object(site, 'service_state', side_effect=[(False, False), (True, True), (False, False)]), \
                    patch.object(site, 'restore_service'), patch.object(site, 'certificate_valid', return_value=True):
                with self.assertRaisesRegex(ValueError, 'installation failed'):
                    with site.manual_activate():
                        self.assertIn('listen 24443 ssl;', config.read_text())
                        self.assertNotIn('listen 80', config.read_text())
                        raise ValueError('installation failed')
            self.assertFalse(config.exists())
            self.assertFalse(unit.exists())
            self.assertFalse((root / 'temporary').exists())
            self.assertEqual(foreign.read_text(), 'preserve this site')
            self.assertTrue(all(call.args[1] == 'chain-panel-https' for call in service.call_args_list))

    def test_manual_https_rejects_foreign_and_linked_temporary_paths(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            site = network.HttpsSite('panel.example.com', 12345)
            site.certificate, site.key = root / 'cert', root / 'key'
            state = root / 'state'
            state.mkdir()
            foreign = root / 'foreign'
            foreign.mkdir()
            before = foreign.stat()
            paths = {'/etc/chain-panel-https': root / 'https', '/var/lib/chain-panel-https': state}
            with patch.object(network, 'Path', side_effect=lambda p: paths.get(p, Path(p))), \
                    patch.object(site, 'certificate_valid', return_value=True), patch.object(network, 'run') as run:
                with self.assertRaisesRegex(ValueError, '不归本安装器管理'):
                    with site.manual_activate():
                        self.fail('foreign directory accepted')
                (state / '.chain-owner').write_text(network.SITE_OWNER + '\n')
                (state / 'proxy').symlink_to(foreign, target_is_directory=True)
                with self.assertRaisesRegex(ValueError, '链接或挂载'):
                    with site.manual_activate():
                        self.fail('linked proxy directory accepted')
                run.assert_not_called()
            after = foreign.stat()
            self.assertEqual((before.st_uid, before.st_gid, before.st_mode, before.st_ctime_ns),
                             (after.st_uid, after.st_gid, after.st_mode, after.st_ctime_ns))

    def test_publish_waits_for_actual_applied_job(self):
        api = legacy.PanelAPI.__new__(legacy.PanelAPI)
        api.call = Mock(side_effect=[{'nodes': [{'report': {'system': {'gateway_version': 2}}}]},
                                    {'job_id': 'job'}, {'jobs': [{'id': 'job', 'state': 'publishing'}]},
                                    {'jobs': [{'id': 'job', 'state': 'applied'}]}])
        with patch.object(legacy.time, 'sleep'):
            api.publish()
        self.assertEqual(api.call.call_count, 4)

    def test_publish_unverified_requires_explicit_confirmation(self):
        for choice in ['1', '2']:
            api = legacy.PanelAPI.__new__(legacy.PanelAPI)
            failure = legacy.urllib.error.HTTPError('http://fixture', 400, 'bad', {},
                io.BytesIO(b'{"error":"REALITY_UNVERIFIED: fixture"}'))
            api.wait_ready = Mock()
            api.call = Mock(side_effect=[failure, {'job_id': 'job'}, {'jobs': [{'id':'job','state':'applied'}]}])
            with patch.object(installer, 'choose', return_value=choice) as choose:
                if choice == '1':
                    api.publish()
                    self.assertEqual(api.call.call_args_list[1].args, ('publish', {'allow_unverified_reality': True}))
                else:
                    with self.assertRaisesRegex(ValueError, '取消切换'):
                        api.publish()
                    self.assertEqual(api.call.call_count, 1)
                self.assertEqual(choose.call_args.args[2], '2')

    def migration(self, temporary, failure=False, pending=False, target_pid=None, domain=None, retained_port=None):
        manager = installer.Manager(Path(temporary))
        etc, data, _ = manager.role_paths('panel')
        etc.mkdir(parents=True)
        data.mkdir(parents=True)
        installer.write_env(etc / 'panel.env', {'CHAIN_LISTEN': '127.0.0.1:23456', 'CHAIN_PUBLIC_URL': 'http://example.com:23456'})
        (data / 'local-system-node').write_text('local')
        with contextlib.closing(sqlite3.connect(data / 'panel.db')) as db:
            db.execute('CREATE TABLE marker(value TEXT)')
            db.execute("INSERT INTO marker VALUES('before')")
            db.commit()
        manager.base.mkdir(parents=True)
        installer.json_write(manager.state_file, {'owner': installer.OWNER, 'roles': {'panel': {}, 'agent': {}}})
        args = installer.make_parser().parse_args(['install'])
        args.domain, args.listen, args.public_url = None, '127.0.0.1:23456', 'http://example.com:23456'
        if domain:
            args.domain, args.public_url = domain, 'https://' + domain
        owner = {'import': True, 'name': 'x-ui', 'pids': [123], 'sources': [{'id': 'old'}]}
        events = []
        result = {'imported': [{'name': 'ok', 'port': 8443, 'protocol': 'vmess'}],
                  'pending': [{'name': 'old ws', 'reason': 'WS needs configuration'}] if pending else []}
        if target_pid is not None:
            result['imported'][0]['reality_target'] = '127.0.0.1:18443'
        api = Mock()
        def call(path, payload=None):
            if path == 'state':
                return {'lines': [], 'pending_imports': [], 'nodes': [{}]}
            if path == 'legacy-import':
                events.append('preview' if payload['preview'] else 'import')
                return result
            return {}
        api.call.side_effect = call
        def publish():
            events.append('publish')
            if failure:
                raise ValueError('publish failed')
        api.publish.side_effect = publish
        api.wait_ready.side_effect = lambda **kwargs: events.append('agent ready')
        handover = Mock(backup=Path(temporary), changed=[])
        def stop():
            events.append('stop old')
            handover.changed.append(owner)
        handover.stop.side_effect = stop
        acme = Path(temporary) / 'chain-proxy-acme.conf'
        def restore():
            self.assertFalse(acme.exists(), 'new port 80 site must be removed before restoring old service')
            if handover.changed:
                events.append('restore old')
        handover.restore.side_effect = restore
        def provision(*args, undo):
            undo.setdefault(acme, None)
            acme.write_text('new HTTP-01 site')
            return True
        @contextlib.contextmanager
        def activate(site):
            events.append('https')
            yield
        with contextlib.ExitStack() as stack:
            for target, name, options in [
                (legacy.shutil, 'which', {'return_value': None}),
                (legacy, 'discover', {'return_value': [owner]}), (legacy, 'select', {'return_value': [owner]}),
                (legacy, 'Handover', {'return_value': handover}), (legacy, 'PanelAPI', {'return_value': api}),
                (installer, 'install_components', {'side_effect': lambda *a: events.append('install')}),
                (network, 'foreign_listeners', {'side_effect': lambda ports=None:
                    'pid=123' if retained_port and (ports is None or retained_port in ports) else ''}),
                (network, 'listeners', {'return_value': f'pid={target_pid}'}),
                (legacy, 'alternate_port', {'return_value': 2443}),
                (network, 'install_proxy_acme', {'side_effect': provision}),
                (network.HttpsSite, 'service_state', {'return_value': (False, False)}),
                (network.HttpsSite, 'activate', {'new': activate}),
                (network.HttpsSite, 'restore_service', {'side_effect': lambda *a: events.append('restore nginx')}),
                (manager.system, 'call', {'side_effect': lambda *a: events.append(a[0] + ' ' + a[1])})]:
                stack.enter_context(patch.object(target, name, **options))
            if target_pid == 123:
                with self.assertRaisesRegex(ValueError, '会随旧代理停止'):
                    legacy.guided_install(manager, None, None, args)
            elif failure:
                with self.assertRaisesRegex(ValueError, 'publish failed'):
                    legacy.guided_install(manager, None, None, args)
            else:
                self.assertTrue(legacy.guided_install(manager, None, None, args))
            if pending:
                handover.stop.assert_not_called()
                api.publish.assert_not_called()
                self.assertTrue(acme.exists())
        return events

    def test_complete_import_precedes_stopping_old_service(self):
        with tempfile.TemporaryDirectory() as temporary:
            events = self.migration(temporary)
        self.assertLess(events.index('install'), events.index('import'))
        self.assertLess(events.index('import'), events.index('stop old'))
        self.assertLess(events.index('agent ready'), events.index('stop old'))
        self.assertLess(events.index('stop old'), events.index('publish'))
        self.assertNotIn('restore old', events)

    def test_pending_import_never_stops_old_service(self):
        with tempfile.TemporaryDirectory() as temporary:
            events = self.migration(temporary, pending=True)
        self.assertEqual(events, ['install', 'preview'])

    def test_pending_import_still_completes_https(self):
        with tempfile.TemporaryDirectory() as temporary:
            events = self.migration(temporary, pending=True, domain='panel.example.com')
            state = installer.Manager(Path(temporary)).state()
            self.assertEqual(state['roles']['panel']['https_port'], 443)
        self.assertIn('https', events)
        self.assertNotIn('stop old', events)
        self.assertNotIn('import', events)

    def test_pending_import_does_not_reuse_retained_proxy_https_port(self):
        with tempfile.TemporaryDirectory() as temporary:
            events = self.migration(temporary, pending=True, domain='panel.example.com', retained_port=443)
            manager = installer.Manager(Path(temporary))
            self.assertEqual(manager.state()['roles']['panel']['https_port'], 2443)
            env = installer.parse_env(manager.role_paths('panel')[0] / 'panel.env')
            self.assertEqual(env['CHAIN_PUBLIC_URL'], 'https://panel.example.com:2443')
        self.assertIn('https', events)
        self.assertNotIn('stop old', events)

    def test_local_reality_target_must_survive_handover(self):
        with tempfile.TemporaryDirectory() as temporary:
            self.assertEqual(self.migration(temporary, target_pid=123), ['install', 'preview'])
        with tempfile.TemporaryDirectory() as temporary:
            self.assertIn('publish', self.migration(temporary, target_pid=456))

    def test_failed_publish_stops_new_agent_before_restoring_old_service(self):
        with tempfile.TemporaryDirectory() as temporary:
            events = self.migration(temporary, failure=True)
        self.assertLess(events.index('stop chain-agent.service'), events.index('restore old'))
        self.assertIn('start chain-panel.service', events)


if __name__ == '__main__':
    unittest.main(verbosity=2)
