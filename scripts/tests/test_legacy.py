"""Migration checks use fixtures and mocked service managers only."""
import contextlib
import json
from pathlib import Path
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
                    validation = root / 'validate.conf'
                    validation.write_text(text)
                    return subprocess.run(['nginx', '-t', '-p', root, '-e', root / 'validation.log', '-c', validation], check=True, capture_output=True)
                return Mock(returncode=0)
            with patch.object(network, 'Path', side_effect=lambda p: root / 'https' if p == '/etc/chain-panel-https' else Path(p)), \
                    patch.object(network, 'service_path', return_value=unit), patch.object(network, 'service_kind', return_value='systemd'), \
                    patch.object(network, 'listeners', return_value=''), patch.object(network, 'install_packages'), \
                    patch.object(network, 'run', side_effect=command), patch.object(network, 'service') as service, \
                    patch.object(site, 'service_state', side_effect=[(False, False), (True, True)]), \
                    patch.object(site, 'restore_service'), patch.object(site, 'certificate_valid', return_value=True):
                with self.assertRaisesRegex(ValueError, 'installation failed'):
                    with site.manual_activate():
                        self.assertIn('listen 24443 ssl;', config.read_text())
                        self.assertNotIn('listen 80', config.read_text())
                        raise ValueError('installation failed')
            self.assertFalse(config.exists())
            self.assertFalse(unit.exists())
            self.assertEqual(foreign.read_text(), 'preserve this site')
            self.assertTrue(all(call.args[1] == 'chain-panel-https' for call in service.call_args_list))

    def test_publish_waits_for_actual_applied_job(self):
        api = legacy.PanelAPI.__new__(legacy.PanelAPI)
        api.call = Mock(side_effect=[{'nodes': [{'report': {'system': {'gateway_version': 2}}}]},
                                    {'job_id': 'job'}, {'jobs': [{'id': 'job', 'state': 'publishing'}]},
                                    {'jobs': [{'id': 'job', 'state': 'applied'}]}])
        with patch.object(legacy.time, 'sleep'):
            api.publish()
        self.assertEqual(api.call.call_count, 4)

    def migration(self, temporary, failure=False):
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
        owner = {'import': True, 'name': 'x-ui', 'pids': [123], 'sources': [{'id': 'old'}]}
        events = []
        result = {'imported': [{'name': 'ok', 'port': 8443, 'protocol': 'vmess'}],
                  'pending': [{'name': 'old ws', 'reason': 'WS needs configuration'}]}
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
        handover = Mock(backup=Path(temporary), changed=[])
        def stop():
            events.append('stop old')
            handover.changed.append(owner)
        handover.stop.side_effect = stop
        acme = Path(temporary) / 'chain-proxy-acme.conf'
        def restore():
            self.assertFalse(acme.exists(), 'new port 80 site must be removed before restoring old service')
            events.append('restore old')
        handover.restore.side_effect = restore
        def provision(*args, undo):
            undo.setdefault(acme, None)
            acme.write_text('new HTTP-01 site')
            return True
        with contextlib.ExitStack() as stack:
            for target, name, options in [
                (legacy, 'discover', {'return_value': [owner]}), (legacy, 'select', {'return_value': [owner]}),
                (legacy, 'Handover', {'return_value': handover}), (legacy, 'PanelAPI', {'return_value': api}),
                (installer, 'install_components', {'side_effect': lambda *a: events.append('install')}),
                (network, 'foreign_listeners', {'return_value': ''}), (network, 'install_proxy_acme', {'side_effect': provision}),
                (network.HttpsSite, 'service_state', {'return_value': (False, False)}),
                (network.HttpsSite, 'restore_service', {'side_effect': lambda *a: events.append('restore nginx')}),
                (manager.system, 'call', {'side_effect': lambda *a: events.append(a[0] + ' ' + a[1])})]:
                stack.enter_context(patch.object(target, name, **options))
            if failure:
                with self.assertRaisesRegex(ValueError, 'publish failed'):
                    legacy.guided_install(manager, None, None, args)
            else:
                self.assertTrue(legacy.guided_install(manager, None, None, args))
        return events

    def test_names_only_do_not_delay_stopping_old_service(self):
        with tempfile.TemporaryDirectory() as temporary:
            events = self.migration(temporary)
        self.assertLess(events.index('install'), events.index('import'))
        self.assertLess(events.index('import'), events.index('stop old'))
        self.assertLess(events.index('stop old'), events.index('publish'))
        self.assertNotIn('restore old', events)

    def test_failed_publish_stops_new_agent_before_restoring_old_service(self):
        with tempfile.TemporaryDirectory() as temporary:
            events = self.migration(temporary, failure=True)
        self.assertLess(events.index('stop chain-agent.service'), events.index('restore old'))
        self.assertIn('start chain-panel.service', events)


if __name__ == '__main__':
    unittest.main(verbosity=2)
