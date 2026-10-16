#!/usr/bin/env python3
"""Delivery tests use an isolated installation tree, never real users/systemd."""
import argparse
import base64
import contextlib
import io
import hashlib
import json
import os
from pathlib import Path
import shutil
import socket
import subprocess
import sys
import tarfile
import tempfile
import time
import unittest
import urllib.error
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import chain_common as common
import chain_install as installer
import chain_release as release
import ci_release as ci


class Delivery(unittest.TestCase):
    def uninstall_fixture(self):
        source, manifest = self.package()
        self.manager.install(source, manifest, self.args(listen='127.0.0.1:22305'))
        self.manager.install(source, manifest, self.args(component='agent', panel_url='http://127.0.0.1:22305',
                            node_id='a94a6794-c60a-4c51-a094-b9164ddaaabc', node_token='s' * 64, allow_http=True))
        for role in installer.ROLES:
            data = self.manager.role_paths(role)[1]
            (data / '.chain-owner').write_text('chain-' + role)
            (data / 'fixture.db').write_bytes(b'private-data-' + role.encode())
        config = self.manager.role_paths('panel')[0] / 'panel.env'
        env = installer.parse_env(config)
        env['CHAIN_PUBLIC_URL'] = 'https://panel.example.com'
        installer.write_env(config, env)
        sites = self.manager.path('/etc/nginx/conf.d')
        sites.mkdir(parents=True)
        for name in ['chain-panel.conf', 'chain-proxy-acme.conf']:
            (sites / name).write_text(installer.SITE_OWNER + '\nserver {}\n')
        (sites / 'other.conf').write_text('keep other website')
        cert = self.manager.path('/etc/letsencrypt/live/chain-panel-panel.example.com/fullchain.pem')
        cert.parent.mkdir(parents=True)
        cert.write_text('keep certificate')
        self.manager.backup_files('panel')
        args = installer.make_parser().parse_args(['uninstall', '--root', str(self.manager.root), '--non-interactive'])
        return source, manifest, args

    def test_uninstall_retains_data_and_reinstall_restores_both_identities(self):
        source, manifest, args = self.uninstall_fixture()
        retained = {p: p.read_bytes() for role in installer.ROLES
                    for directory in self.manager.role_paths(role)[:2] for p in directory.rglob('*') if p.is_file()}
        self.manager.operate(args)
        state = self.manager.state()
        self.assertFalse(state['roles'])
        self.assertEqual(state['retained_roles']['panel']['domain'], 'panel.example.com')
        for p, body in retained.items():
            self.assertEqual(p.read_bytes(), body)
        for role in installer.ROLES:
            self.assertFalse(self.manager.role_paths(role)[2].exists())
        for name in ['manager', 'releases', 'current-panel', 'current-agent']:
            self.assertFalse((self.manager.base / name).exists())
        self.assertFalse(self.manager.path('/usr/local/bin/chainctl').exists())
        self.assertTrue(self.manager.path('/etc/nginx/conf.d/other.conf').exists())
        self.assertFalse(self.manager.path('/etc/nginx/conf.d/chain-panel.conf').exists())
        self.manager.install(source, manifest, self.args())
        with patch.object(installer.urllib.request, 'build_opener', side_effect=AssertionError('must reuse saved Agent identity')):
            installer.provision_local_agent(self.manager, source, manifest, self.args())
        self.assertEqual(set(self.manager.state()['roles']), {'panel', 'agent'})
        self.assertFalse(self.manager.state()['retained_roles'])
        for p, body in retained.items():
            self.assertEqual(p.read_bytes(), body)

    def test_purge_can_remove_retained_installation_without_deleting_certificate_or_other_sites(self):
        _, _, args = self.uninstall_fixture()
        self.manager.operate(args)
        args.purge = True
        self.manager.operate(args)
        for role in installer.ROLES:
            for p in self.manager.role_paths(role):
                self.assertFalse(p.exists())
        for path in ['/opt/chain', '/var/lib/chain-acme', '/var/backups/chain-panel', '/usr/local/bin/chainctl']:
            self.assertFalse(self.manager.path(path).exists())
        self.assertEqual(self.manager.path('/etc/nginx/conf.d/other.conf').read_text(), 'keep other website')
        self.assertEqual(self.manager.path('/etc/letsencrypt/live/chain-panel-panel.example.com/fullchain.pem').read_text(),
                         'keep certificate')

    def test_uninstall_rejects_foreign_service_and_live_process_before_deleting_data(self):
        _, _, args = self.uninstall_fixture()
        unit = self.manager.role_paths('panel')[2]
        original = unit.read_bytes()
        unit.write_text('foreign service')
        with patch.object(self.manager.system, 'call') as call:
            with self.assertRaisesRegex(ValueError, '不归本安装器'):
                self.manager.operate(args)
            call.assert_not_called()
        unit.write_bytes(original)
        with patch.object(self.manager.system, 'active', return_value=True):
            with self.assertRaisesRegex(ValueError, '尚未停止'):
                self.manager.operate(args)
        self.assertEqual(set(self.manager.state()['roles']), {'panel', 'agent'})
        self.assertTrue(self.manager.path('/usr/local/bin/chainctl').exists())
        self.assertTrue((self.manager.role_paths('panel')[1] / 'fixture.db').exists())

    def test_uninstall_menu_requires_confirmation_and_exits_after_removing_manager(self):
        self.uninstall_fixture()
        with patch.object(installer, 'choose', side_effect=['16', '1']), patch('builtins.input', return_value='UNINSTALL'):
            installer.menu(self.manager.root)
        self.assertFalse(self.manager.path('/usr/local/bin/chainctl').exists())
        args = installer.make_parser().parse_args(['uninstall', '--root', str(self.manager.root), '--purge'])
        with patch('builtins.input', return_value='UNINSTALL'):
            with self.assertRaisesRegex(ValueError, '取消'):
                self.manager.operate(args)
        self.assertTrue(self.manager.role_paths('panel')[1].exists())

    def test_uninstall_restores_nginx_files_when_validation_fails(self):
        _, _, args = self.uninstall_fixture()
        self.manager.live = True
        site = self.manager.path('/etc/nginx/conf.d/chain-panel.conf')
        before = site.read_bytes()
        with patch.object(installer, 'nginx_conf', return_value=self.manager.path('/etc/nginx/nginx.conf')), \
                patch.object(installer.shutil, 'which', return_value='/usr/sbin/nginx'), \
                patch.object(installer, 'run', side_effect=subprocess.CalledProcessError(1, ['nginx', '-t'])):
            with self.assertRaises(subprocess.CalledProcessError):
                self.manager.operate(args)
        self.assertEqual(site.read_bytes(), before)
        self.assertEqual(set(self.manager.state()['roles']), {'panel', 'agent'})
        self.assertTrue(self.manager.path('/usr/local/bin/chainctl').exists())

    def test_reinstall_restores_saved_https_before_installing_panel(self):
        source, _, args = self.uninstall_fixture()
        archive = self.archive(source)
        self.manager.operate(args)
        args = installer.make_parser().parse_args(['install', '--package', str(archive), '--panel-only'])
        self.manager.live = True
        def questions(manager, values):
            self.assertEqual(values.domain, 'panel.example.com')
            values.listen = '127.0.0.1:22305'
        with patch.object(installer, 'panel_questions', side_effect=questions), \
                patch.object(installer, 'HttpsSite') as https, \
                patch.object(self.manager, 'install') as install, patch.object(self.manager, 'show_access'):
            https.return_value.activate.return_value = contextlib.nullcontext()
            installer.install_package(self.manager, args)
        https.assert_called_once_with('panel.example.com', 22305, False)
        install.assert_called_once()

    def test_uninstall_restores_other_sites_before_removing_unified_ingress(self):
        from chain_ingress import Ingress
        self.uninstall_fixture()
        ingress = Ingress(self.manager.root)
        site = self.manager.path('/etc/nginx/conf.d/other.conf')
        before, after = 'server { listen 443 ssl; }', 'server { listen 127.0.0.1:14443 ssl proxy_protocol; }'
        site.write_text(after)
        ingress.config.mkdir(parents=True)
        for name in ['stream.conf', 'routes.conf']:
            (ingress.config / name).write_text('fixture')
        common.json_write(ingress.state_path, {'files': {str(site): {'before': before, 'after': after, 'mode': 0o644}}})
        self.manager.live = True
        args = installer.make_parser().parse_args(['uninstall', '--non-interactive'])
        with patch.object(installer, 'nginx_conf', return_value=self.manager.path('/etc/nginx/nginx.conf')), \
                patch.object(Ingress, 'command', return_value=''), patch.object(installer, 'run'), \
                patch('chain_ingress.service'), \
                patch.object(installer, 'service_state', return_value=(False, False)):
            self.manager.operate(args)
        self.assertEqual(site.read_text(), before)
        self.assertFalse(ingress.config.exists())
        self.assertFalse(ingress.directory.exists())

    def test_backup_preserves_certbot_links_but_rejects_escape(self):
        source, manifest = self.package()
        self.manager.install(source, manifest, self.args())
        data = self.manager.role_paths('panel')[1]
        archive = data / 'acme/config/archive/proxy.example'
        live = data / 'acme/config/live/proxy.example'
        archive.mkdir(parents=True)
        live.mkdir(parents=True)
        (archive / 'cert1.pem').write_text('fixture certificate')
        link = live / 'cert.pem'
        link.symlink_to('../../archive/proxy.example/cert1.pem')
        backup = self.manager.backup_files('panel')
        with tarfile.open(backup) as bundle:
            member = bundle.getmember('data/acme/config/live/proxy.example/cert.pem')
            self.assertTrue(member.issym())
            self.assertEqual(member.linkname, '../../archive/proxy.example/cert1.pem')
        link.unlink()
        link.symlink_to('/etc/passwd')
        with self.assertRaisesRegex(ValueError, 'Certbot'):
            self.manager.backup_files('panel')

    def test_local_security_recovery_preserves_token_and_business_data(self):
        import sqlite3
        source, manifest = self.package()
        self.manager.install(source, manifest, self.args())
        data = self.manager.role_paths('panel')[1]
        (data / 'admin-token').write_text('keep-this-token')
        document = {'security': {'totp_secret': 'old', 'passkeys': [{'id': 'old'}]},
                    'lines': [{'id': 'unchanged-line'}], 'subscriptions': [{'id': 'keep'}]}
        with contextlib.closing(sqlite3.connect(data / 'panel.db')) as connection, connection:
            connection.execute('CREATE TABLE state(id INTEGER PRIMARY KEY, body TEXT NOT NULL)')
            connection.execute('INSERT INTO state VALUES(1,?)', (json.dumps(document),))
        self.manager.operate(self.args(action='reset-auth', non_interactive=True))
        with contextlib.closing(sqlite3.connect(data / 'panel.db')) as connection:
            restored = json.loads(connection.execute('SELECT body FROM state WHERE id=1').fetchone()[0])
        self.assertEqual(restored, {**document, 'security': {}})
        self.assertEqual((data / 'admin-token').read_text(), 'keep-this-token')

    def setUp(self):
        for mock in [patch.object(installer, 'service_kind', return_value='systemd'),
                     patch.object(installer, 'service_path', side_effect=lambda name:
                         Path('/etc/systemd/system') / (name + '.service'))]:
            mock.start()
            self.addCleanup(mock.stop)
        self.temp = tempfile.TemporaryDirectory(prefix='chain-delivery-test-')
        self.root = Path(self.temp.name)
        self.manager = installer.Manager(self.root / 'system')

    def tearDown(self):
        self.temp.cleanup()
        self.assertFalse(self.root.exists())

    def args(self, **kwargs):
        result = argparse.Namespace(action='install', component='panel', listen=None, secure_cookie=None,
                                    panel_url=None, node_id=None, token_file=None, allow_http=False)
        for key, value in kwargs.items():
            setattr(result, key, value)
        return result

    def package(self, version='0.1.0'):
        root = self.root / ('stage-' + version)
        (root / 'bin').mkdir(parents=True)
        (root / 'scripts').mkdir()
        for name in ['chain-panel', 'chain-agent', 'xray']:
            (root / 'bin' / name).write_text('fixture-' + version)
        for name in ['chain_common.py', 'chain_install.py', 'chain_network.py', 'chain_ingress.py', 'chain_platform.py', 'install-server.sh']:
            shutil.copy2(Path(__file__).resolve().parents[1] / name, root / 'scripts' / name)
        files = {str(p.relative_to(root)): {'sha256': common.sha256(p), 'mode': 0o755 if p.parent.name == 'bin' or p.suffix == '.sh' else 0o644}
                 for p in root.rglob('*') if p.is_file()}
        manifest = {'format': 1, 'version': version, 'arch': common.architecture(), 'os': common.operating_system(), 'libc': 'musl', 'macos_min': '14.0', 'files': files}
        common.json_write(root / 'release.json', manifest)
        return root, manifest

    def archive(self, root):
        archive = self.root / (root.name + '.tar.gz')
        with tarfile.open(archive, 'w:gz') as bundle:
            for p in root.rglob('*'):
                if p.is_file():
                    bundle.add(p, arcname=str(p.relative_to(root)), recursive=False)
        (archive.parent / 'SHA256SUMS').write_text(common.sha256(archive) + '  ' + archive.name + '\n')
        return archive

    def test_verified_archive_and_checksum_tampering(self):
        source, manifest = self.package()
        archive = self.archive(source)
        common.verify_archive_checksum(archive)
        self.assertEqual(common.safe_extract(archive, self.root / 'extract'), manifest)
        archive.write_bytes(archive.read_bytes() + b'tampered')
        with self.assertRaisesRegex(ValueError, 'SHA256'):
            common.verify_archive_checksum(archive)

    def test_archive_rejects_escape_symlink_and_duplicate(self):
        for variant in ('escape', 'symlink', 'duplicate'):
            archive = self.root / (variant + '.tar.gz')
            with tarfile.open(archive, 'w:gz') as bundle:
                entry = tarfile.TarInfo('../outside' if variant == 'escape' else 'file')
                entry.size = 1
                if variant == 'symlink':
                    entry.type = tarfile.SYMTYPE
                    entry.linkname = '/etc/passwd'
                bundle.addfile(entry, io.BytesIO(b'x'))
                if variant == 'duplicate':
                    bundle.addfile(entry, io.BytesIO(b'y'))
            with self.assertRaises(ValueError):
                common.safe_extract(archive, self.root / variant)
        self.assertFalse((self.root / 'outside').exists())

    def test_file_manifest_tampering(self):
        source, _ = self.package()
        (source / 'bin/chain-panel').write_text('altered')
        with self.assertRaisesRegex(ValueError, '校验失败'):
            common.verify_tree(source)

    def test_connection_code_validates_and_does_not_echo_secrets(self):
        document = {'panel_url': 'https://panel.example.com', 'node_id': 'a94a6794-c60a-4c51-a094-b9164ddaaabc', 'token': 's' * 64}
        def encode(value):
            return 'chain-agent-v1:' + base64.urlsafe_b64encode(json.dumps(value).encode()).decode().rstrip('=')
        self.assertEqual(installer.decode_connection_code(encode(document)), document)
        for value in [encode({**document, 'node_id': 'invalid'}), encode({**document, 'panel_url': 'https://user:pass@host/'}), 'chain-agent-v1:broken']:
            with self.assertRaisesRegex(ValueError, '接入码无效') as error:
                installer.decode_connection_code(value)
            self.assertNotIn(document['token'], str(error.exception))

    def test_guided_panel_and_agent_install_without_manual_env(self):
        source, _ = self.package()
        archive = self.archive(source)
        args = installer.make_parser().parse_args(['install', '--root', str(self.manager.root), '--package', str(archive)])
        with patch('builtins.input', side_effect=['']), patch.object(installer, 'public_ipv4', return_value='192.0.2.1'), \
                patch.object(installer, 'available_port', return_value=48099):
            installer.install_wizard(self.manager, args)
        self.assertEqual(installer.parse_env(self.manager.role_paths('panel')[0] / 'panel.env')['CHAIN_PUBLIC_URL'], 'http://192.0.2.1:48099')
        document = {'panel_url': 'https://panel.example.com', 'node_id': 'a94a6794-c60a-4c51-a094-b9164ddaaabc', 'token': 's' * 64}
        code = 'chain-agent-v1:' + base64.urlsafe_b64encode(json.dumps(document).encode()).decode().rstrip('=')
        agent = installer.make_parser().parse_args(['install', '--component', 'agent', '--root', str(self.manager.root), '--package', str(archive)])
        with patch('getpass.getpass', return_value=code):
            installer.install_wizard(self.manager, agent)
        self.assertEqual((self.manager.role_paths('agent')[0] / 'token').read_text().strip(), document['token'])
        self.assertEqual(set(self.manager.state()['roles']), {'panel', 'agent'})

    def test_help_and_noninteractive_menu_do_not_require_root(self):
        with patch('os.geteuid', return_value=1000), patch('sys.stdin.isatty', return_value=False), contextlib.redirect_stdout(io.StringIO()) as stream:
            installer.main(['help'])
            installer.main([])
        self.assertIn('打开交互管理菜单', stream.getvalue())
        self.assertIn('--non-interactive', stream.getvalue())

    def test_first_setup_has_one_domain_prompt_and_repeat_preserves_access(self):
        source, _ = self.package()
        archive = self.archive(source)
        argv = ['setup', '--root', str(self.manager.root), '--package', str(archive)]
        with patch('sys.stdin.isatty', return_value=True), patch('builtins.input', return_value='') as prompt, \
                patch.object(installer, 'public_ipv4', return_value='192.0.2.1'), \
                patch.object(installer, 'available_port', return_value=45123):
            installer.main(argv)
        self.assertEqual(prompt.call_count, 1)
        config = self.manager.role_paths('panel')[0] / 'panel.env'
        original = config.read_bytes()
        with patch('sys.stdin.isatty', return_value=True), patch('builtins.input', side_effect=AssertionError('no repeated setup')):
            installer.main(argv)
        self.assertEqual(config.read_bytes(), original)
        self.assertEqual(installer.parse_env(config)['CHAIN_PUBLIC_URL'], 'http://192.0.2.1:45123')

    def test_unavailable_release_fails_before_any_input(self):
        args = installer.make_parser().parse_args(['install', '--latest', '--root', str(self.manager.root), '--work-parent', str(self.root)])
        with patch.object(installer, 'download_release', side_effect=ValueError('no release')), \
                patch('builtins.input', side_effect=AssertionError('must check release before questions')):
            with self.assertRaisesRegex(ValueError, 'no release'):
                installer.install_wizard(self.manager, args)
        self.assertFalse(list(self.root.glob('chain-install-*')))

    def test_repeated_setup_retries_missing_default_agent(self):
        source, manifest = self.package()
        self.manager.install(source, manifest, self.args())
        self.manager.live = True
        with patch.object(installer, 'Manager', return_value=self.manager), patch('sys.stdin.isatty', return_value=True), \
                patch.object(installer, 'install_wizard') as wizard:
            installer.main(['setup', '--root', str(self.manager.root)])
        wizard.assert_called_once()
        self.assertEqual(wizard.call_args.args[1].action, 'install')

    def test_https_port_change_checks_after_stopping_owned_panel(self):
        source, manifest = self.package()
        self.manager.install(source, manifest, self.args(listen='0.0.0.0:45123'))
        self.manager.live = True
        events = []
        with patch.object(self.manager, 'account', return_value=(os.geteuid(), os.getegid())), \
                patch('chain_network.install_proxy_acme'), \
                patch.object(self.manager.system, 'active', return_value=True), \
                patch.object(self.manager.system, 'autostart', return_value=True), \
                patch.object(self.manager.system, 'call', side_effect=lambda *args, **kwargs: events.append(args[0])), \
                patch.object(self.manager, 'healthy'), patch.object(self.manager, 'show_access'), \
                patch.object(self.manager, 'check_panel_port', side_effect=lambda env: events.append('bind')):
            self.manager.install(source, manifest, self.args(listen='127.0.0.1:45123', secure_cookie='1'))
        self.assertLess(events.index('stop'), events.index('bind'))
        self.assertLess(events.index('bind'), events.index('restart'))

    def test_common_run_allows_best_effort_cleanup(self):
        self.assertEqual(common.run([sys.executable, '-c', 'raise SystemExit(3)'], check=False).returncode, 3)

    def test_online_download_missing_release_and_integrity_failure(self):
        request_error = urllib.error.HTTPError('https://api.github.com/', 404, 'Not found', {}, None)
        with patch('urllib.request.urlopen', side_effect=request_error):
            with self.assertRaisesRegex(ValueError, '尚无已发布'):
                common.download_release(self.root)
        name = f'chain-panel-0.1.0-{common.operating_system()}-{common.architecture()}.tar.gz'
        release_info = {'tag_name': 'v0.1.0', 'assets': [{'name': n, 'browser_download_url': f'https://github.com/{common.RELEASE_REPO}/releases/download/v0.1.0/{n}'} for n in [name, 'SHA256SUMS']]}
        class Response(io.BytesIO):
            def geturl(self):
                return 'https://release-assets.githubusercontent.com/fixture'
        def fetch(request, **_):
            url = request.full_url if hasattr(request, 'full_url') else request
            if 'api.github' in url:
                return Response(json.dumps(release_info).encode())
            return Response((('0' * 64 + '  ' + name + '\n').encode()) if url.endswith('SHA256SUMS') else b'tampered archive')
        args = installer.make_parser().parse_args(['install', '--latest', '--root', str(self.manager.root), '--work-parent', str(self.root)])
        with patch('urllib.request.urlopen', side_effect=fetch):
            with self.assertRaisesRegex(ValueError, 'SHA256'):
                installer.install_package(self.manager, args)
        self.assertFalse(self.manager.base.exists())
        self.assertFalse(list(self.root.glob('chain-install-*')))

    def test_update_downloads_release_without_explicit_package(self):
        source, manifest = self.package()
        self.manager.install(source, manifest, self.args())
        source2, _ = self.package('0.2.0')
        archive = self.archive(source2)
        args = installer.make_parser().parse_args(['update', '--root', str(self.manager.root), '--work-parent', str(self.root)])
        with patch.object(installer, 'download_release', return_value=(archive, '0.2.0')) as download:
            installer.install_package(self.manager, args)
        download.assert_called_once()
        self.assertEqual(self.manager.state()['roles']['panel']['version'], '0.2.0')

    def test_existing_local_agent_is_not_registered_again(self):
        source, manifest = self.package()
        token = self.root / 'token'
        token.write_text('a' * 64)
        token.chmod(0o600)
        self.manager.install(source, manifest, self.args(component='agent', panel_url='https://panel.example.com', node_id='a94a6794-c60a-4c51-a094-b9164ddaaabc', token_file=token))
        with patch('urllib.request.build_opener', side_effect=AssertionError('must not re-register')):
            installer.provision_local_agent(self.manager, source, manifest, self.args())

    def test_panel_upgrade_marks_only_existing_matching_local_agent(self):
        source, manifest = self.package()
        self.manager.install(source, manifest, self.args())
        node_id = 'a94a6794-c60a-4c51-a094-b9164ddaaabc'
        token = self.root / 'token'
        token.write_text('a' * 64)
        token.chmod(0o600)
        self.manager.install(source, manifest, self.args(component='agent',
            panel_url='http://127.0.0.1:8088', node_id=node_id, token_file=token))
        marker = self.manager.role_paths('panel')[1] / 'local-system-node'
        panel_env_path = self.manager.role_paths('panel')[0] / 'panel.env'
        agent_env_path = self.manager.role_paths('agent')[0] / 'agent.env'
        for listen, url, expected in [
                ('127.0.0.1:8088', 'http://127.0.0.1:8088', True),
                ('0.0.0.0:8088', 'http://localhost:8088', True),
                ('192.0.2.10:8088', 'http://192.0.2.10:8088', True),
                ('192.0.2.10:8088', 'http://127.0.0.1:8088', False),
                ('127.0.0.1:8088', 'http://127.0.0.1:8099', False),
                ('127.0.0.1:8088', 'https://remote.example.com:8088', False)]:
            with self.subTest(listen=listen, url=url):
                marker.unlink(missing_ok=True)
                installer.write_env(panel_env_path, {'CHAIN_LISTEN': listen})
                installer.write_env(agent_env_path, {'CHAIN_PANEL_URL': url, 'CHAIN_NODE_ID': node_id})
                with patch('urllib.request.build_opener', side_effect=AssertionError('must not register')):
                    installer.provision_local_agent(self.manager, source, manifest, self.args(action='update'))
                self.assertEqual(marker.exists(), expected)
                if expected:
                    self.assertEqual(marker.read_text().strip(), node_id)
                    recovery = json.loads(marker.with_name('local-system-identity.json').read_text())
                    self.assertEqual(recovery, {'id': node_id, 'token_hash': hashlib.sha256(b'a' * 64).hexdigest()})
        with patch.object(self.manager, 'live', True), patch.object(self.manager, 'install'), \
                patch.object(installer, 'provision_local_agent') as provision:
            installer.install_components(self.manager, source, manifest, self.args(action='update', panel_only=True))
            provision.assert_called_once()
        with patch.object(self.manager, 'live', True), patch.object(self.manager, 'install'), \
                patch.object(self.manager, 'state', return_value={'roles': {'panel': {}}}), \
                patch.object(installer, 'provision_local_agent') as provision:
            installer.install_components(self.manager, source, manifest, self.args(action='update', panel_only=True))
            provision.assert_not_called()

    def test_local_agent_auto_registration_and_failed_install_cleanup(self):
        source, manifest = self.package()
        self.manager.install(source, manifest, self.args(secure_cookie='1'))
        (self.manager.role_paths('panel')[1] / 'admin-token').write_text('fixture-admin-token')
        node = {'id': 'a94a6794-c60a-4c51-a094-b9164ddaaabc', 'token': 's' * 64}
        paths = []
        class Response(io.BytesIO):
            headers = {'Set-Cookie': 'chain_session=fixture; Path=/; Secure'}
        class Opener:
            def open(self, request, **_):
                path = request.full_url.split('/api/')[1]
                paths.append((request.method, path))
                if path != 'login':
                    assert request.get_header('Cookie') == 'chain_session=fixture'
                if path == 'nodes':
                    assert json.loads(request.data)['host'] == '192.0.2.1'
                return Response(json.dumps(node if path == 'nodes' else {'ok': True}).encode())
        args = self.args(local_host='192.0.2.1', local_target='tls.example.com:443')
        with patch('urllib.request.build_opener', return_value=Opener()):
            with patch.object(self.manager, 'install', side_effect=ValueError('fixture startup failure')):
                with self.assertRaisesRegex(ValueError, 'startup failure'):
                    installer.provision_local_agent(self.manager, source, manifest, args)
            self.assertIn(('DELETE', 'nodes/' + node['id']), paths)
            self.assertEqual(paths[-1], ('POST', 'logout'))
            installer.provision_local_agent(self.manager, source, manifest, args)
        env = installer.parse_env(self.manager.role_paths('agent')[0] / 'agent.env')
        self.assertEqual(env['CHAIN_NODE_ID'], node['id'])
        self.assertEqual(env['CHAIN_PANEL_URL'], 'http://127.0.0.1:8088')
        self.assertEqual((self.manager.role_paths('agent')[0] / 'token').read_text().strip(), node['token'])

    def test_install_upgrade_backup_and_rollback_preserve_state(self):
        source, manifest = self.package()
        self.manager.install(source, manifest, self.args(listen='127.0.0.1:8099', secure_cookie='1'))
        initial = self.manager.state()['roles']['panel']['current']
        etc, data, _ = self.manager.role_paths('panel')
        (data / 'admin-token').write_text('private-fixture')
        (data / 'panel.db').write_bytes(b'database-fixture')
        source2, manifest2 = self.package('0.2.0')
        self.manager.install(source2, manifest2, self.args(action='update'))
        state = self.manager.state()['roles']['panel']
        self.assertEqual(state['previous'], initial)
        self.assertEqual(installer.parse_env(etc / 'panel.env')['CHAIN_LISTEN'], '127.0.0.1:8099')
        self.assertEqual((data / 'panel.db').read_bytes(), b'database-fixture')
        self.assertEqual((etc / 'panel.env').stat().st_mode & 0o777, 0o600)
        backups = list(self.manager.path('/var/backups/chain-panel').glob('*.tar.gz'))
        self.assertEqual(len(backups), 1)
        self.assertEqual(backups[0].stat().st_mode & 0o777, 0o600)
        with tarfile.open(backups[0]) as bundle:
            self.assertEqual(bundle.extractfile('data/panel.db').read(), b'database-fixture')
        self.manager.rollback('panel')
        self.assertEqual(self.manager.state()['roles']['panel']['current'], initial)
        self.assertEqual((data / 'admin-token').read_text(), 'private-fixture')

    def test_failed_upgrade_restores_links_config_and_state(self):
        source, manifest = self.package()
        self.manager.install(source, manifest, self.args())
        original = self.manager.state()
        config = self.manager.role_paths('panel')[0] / 'panel.env'
        original_config = config.read_bytes()
        source2, manifest2 = self.package('0.2.0')
        with patch.object(self.manager, 'healthy', side_effect=ValueError('fixture startup failure')):
            with self.assertRaisesRegex(ValueError, 'startup failure'):
                self.manager.install(source2, manifest2, self.args(action='update', listen='127.0.0.1:9090'))
        self.assertEqual(self.manager.state(), original)
        self.assertEqual(config.read_bytes(), original_config)
        self.assertEqual((self.manager.base / 'current-panel').resolve().name, original['roles']['panel']['current'])

    def test_foreign_installation_and_service_are_not_overwritten(self):
        source, manifest = self.package()
        unit = self.manager.role_paths('panel')[2]
        unit.parent.mkdir(parents=True)
        unit.write_text('unrelated existing service')
        with self.assertRaisesRegex(ValueError, '拒绝覆盖'):
            self.manager.install(source, manifest, self.args())
        self.assertEqual(unit.read_text(), 'unrelated existing service')
        self.assertFalse(self.manager.base.exists())
        unit.unlink()
        self.manager.base.mkdir(parents=True)
        (self.manager.base / 'unrelated').write_text('keep')
        with self.assertRaisesRegex(ValueError, '拒绝覆盖'):
            self.manager.install(source, manifest, self.args())
        self.assertEqual((self.manager.base / 'unrelated').read_text(), 'keep')

    def test_agent_identity_requirements_and_role_isolation(self):
        source, manifest = self.package()
        self.manager.install(source, manifest, self.args())
        panel = self.manager.state()['roles']['panel'].copy()
        with self.assertRaisesRegex(ValueError, '需要 --panel-url'):
            self.manager.install(source, manifest, self.args(component='agent'))
        token = self.root / 'node-token'
        token.write_text('a' * 64)
        token.chmod(0o600)
        args = self.args(component='agent', panel_url='https://panel.example.com', node_id='a94a6794-c60a-4c51-a094-b9164ddaaabc', token_file=token)
        self.manager.install(source, manifest, args)
        self.assertEqual(self.manager.state()['roles']['panel'], panel)
        etc, _, unit = self.manager.role_paths('agent')
        self.assertIn('LimitNOFILE=65536', unit.read_text().splitlines())
        self.assertNotIn('LimitNOFILE=', self.manager.role_paths('panel')[2].read_text())
        self.assertEqual((etc / 'token').read_text().strip(), 'a' * 64)
        self.assertEqual((etc / 'token').stat().st_mode & 0o777, 0o600)
        with self.assertRaisesRegex(ValueError, '需要 HTTPS'):
            self.manager.config('agent', self.args(component='agent', panel_url='http://10.0.0.2:8088', node_id=args.node_id))

    def test_bad_arch_and_glibc_fail_before_changes(self):
        source, manifest = self.package()
        for wrong in [{'arch': 'unsupported'}, {'os': 'linux', 'libc': 'glibc', 'glibc_min': '999.0'}]:
            with self.assertRaises(ValueError):
                self.manager.install(source, {**manifest, **wrong}, self.args())
            self.assertFalse(self.manager.base.exists())

    @unittest.skipUnless(shutil.which('systemd-analyze'), 'systemd-analyze 未安装')
    def test_service_units_parse_without_ignored_environment_files(self):
        manager = installer.Manager(self.root / 'system with spaces')
        for role in installer.ROLES:
            binary = manager.base / ('current-' + role) / 'bin' / ('chain-' + role)
            binary.parent.mkdir(parents=True)
            shutil.copy2('/bin/true', binary)
            unit = manager.role_paths(role)[2]
            unit.parent.mkdir(parents=True, exist_ok=True)
            unit.write_text(manager.unit(role))
            result = subprocess.run(['systemd-analyze', 'verify', str(unit)], capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertNotIn('EnvironmentFile=', result.stderr)
            self.assertNotIn('Failed to parse', result.stderr)

    def test_version_updates_all_four_manifests(self):
        repo = self.root / 'repo'
        for name in ['panel/Cargo.toml', 'panel/Cargo.lock', 'panel/frontend/package.json', 'panel/frontend/package-lock.json']:
            destination = repo / name
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(release.ROOT / name, destination)
        with patch.object(release, 'ROOT', repo):
            release.set_version('0.3.7')
            self.assertEqual(release.version(), '0.3.7')
            before = (repo / 'panel/Cargo.toml').read_bytes()
            with self.assertRaises(ValueError):
                release.set_version('bad; version')
            self.assertEqual((repo / 'panel/Cargo.toml').read_bytes(), before)

    def release_repo(self):
        repo = self.root / 'release-repo'
        repo.mkdir()
        def git(*args):
            return common.output(['git', *args], cwd=repo)
        git('init', '-b', 'main')
        git('config', 'user.name', 'Release Test')
        git('config', 'user.email', 'release@example.invalid')
        git('config', 'commit.gpgsign', 'false')
        git('config', 'tag.gpgsign', 'false')
        for name in ['panel/Cargo.toml', 'panel/Cargo.lock', 'panel/frontend/package.json', 'panel/frontend/package-lock.json']:
            destination = repo / name
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(release.ROOT / name, destination)
        for name in ['README.md', '.gitignore', 'scripts/fixture.py', '.github/fixture.yml', 'unrelated.txt']:
            destination = repo / name
            destination.parent.mkdir(parents=True, exist_ok=True)
            destination.write_text('fixture\n')
        git('add', '.')
        git('commit', '-m', '测试初始提交')
        remote = self.root / 'origin.git'
        common.output(['git', 'init', '--bare', remote])
        git('remote', 'add', 'origin', str(remote))
        git('push', 'origin', 'main')
        return repo, git

    def test_release_wizard_cancel_publish_and_retry(self):
        repo, git = self.release_repo()
        (repo / 'panel/new.txt').write_text('new\n')
        git('add', 'panel/new.txt')
        git('commit', '-m', '提交业务改动')
        (repo / 'unrelated.txt').write_text('keep\n')
        (repo / 'theme.zip').write_bytes(b'keep')
        answers = ['', 'y', '']
        with patch.object(release, 'ROOT', repo), patch.object(release.sys.stdin, 'isatty', return_value=True):
            before = git('rev-parse', 'HEAD')
            old = release.version()
            parts = old.split('.')
            expected = '.'.join([*parts[:2], str(int(parts[2]) + 1)])
            tag = 'v' + expected
            with patch('builtins.input', side_effect=['', 'n']):
                release.release_wizard()
            self.assertEqual(release.version(), old)
            self.assertEqual(git('rev-parse', 'HEAD'), before)
            self.assertFalse(git('diff', '--cached', '--name-only'))
            original_run = release.run
            def fail_push(command, **kwargs):
                if command[:2] == ['git', 'push']:
                    raise subprocess.CalledProcessError(1, command)
                return original_run(command, **kwargs)
            with patch('builtins.input', side_effect=answers), patch.object(release, 'run', side_effect=fail_push):
                with self.assertRaises(subprocess.CalledProcessError):
                    release.release_wizard()
            self.assertEqual(release.version(), expected)
            commit = git('rev-parse', 'HEAD')
            self.assertEqual(git('rev-parse', tag + '^{commit}'), commit)
            self.assertNotIn(tag, git('ls-remote', '--tags', 'origin'))
            with patch('builtins.input', return_value='y') as prompt:
                release.release_wizard()
            self.assertEqual(prompt.call_count, 1)
            self.assertEqual(git('rev-parse', 'HEAD'), commit)
            self.assertIn(commit + '\trefs/heads/main', git('ls-remote', 'origin', 'refs/heads/main'))
            self.assertIn(commit + '\trefs/tags/' + tag + '^{}', git('ls-remote', '--tags', 'origin'))
            names = git('show', '--pretty=format:', '--name-only', 'HEAD')
            self.assertEqual(set(names.splitlines()), {'panel/Cargo.toml', 'panel/Cargo.lock',
                                                     'panel/frontend/package.json', 'panel/frontend/package-lock.json'})
            self.assertEqual(git('show', 'HEAD:panel/new.txt'), 'new')
            self.assertNotIn('unrelated.txt', names)
            self.assertNotIn('theme.zip', names)
            self.assertIn('背景与目的', git('log', '-1', '--format=%B'))
            self.assertIn('unrelated.txt', git('status', '--porcelain'))

    def test_release_wizard_rejects_staging_and_existing_tag(self):
        repo, git = self.release_repo()
        with patch.object(release, 'ROOT', repo), patch.object(release.sys.stdin, 'isatty', return_value=True):
            (repo / 'unrelated.txt').write_text('staged\n')
            git('add', 'unrelated.txt')
            with self.assertRaisesRegex(ValueError, '暂存区'):
                release.release_wizard()
            self.assertEqual(git('diff', '--cached', '--name-only'), 'unrelated.txt')
            git('reset', 'HEAD', '--', 'unrelated.txt')
            (repo / 'panel/pending.txt').write_text('not committed\n')
            with patch('builtins.input', side_effect=AssertionError('must reject before prompting')):
                with self.assertRaisesRegex(ValueError, '请先提交发行源码'):
                    release.release_wizard()
            (repo / 'panel/pending.txt').unlink()
            git('config', 'remote.origin.pushurl', str(self.root / 'different.git'))
            with self.assertRaisesRegex(ValueError, '拉取与推送地址不同'):
                release.release_wizard()
            git('config', '--unset', 'remote.origin.pushurl')
            git('tag', 'v99.0.0')
            with patch('builtins.input', return_value='99.0.0'):
                with self.assertRaisesRegex(ValueError, '标签已经存在'):
                    release.release_wizard()

    def test_release_wizard_custom_version(self):
        repo, git = self.release_repo()
        with patch.object(release, 'ROOT', repo), patch.object(release.sys.stdin, 'isatty', return_value=True):
            with patch('builtins.input', side_effect=['98.1.0', 'y', '']):
                release.release_wizard()
            self.assertEqual(release.version(), '98.1.0')
            commit = git('rev-parse', 'HEAD')
            self.assertIn(commit + '\trefs/tags/v98.1.0^{}', git('ls-remote', '--tags', 'origin'))

    def test_release_skip_then_push_existing_version(self):
        repo, git = self.release_repo()
        with patch.object(release, 'ROOT', repo), patch.object(release.sys.stdin, 'isatty', return_value=True):
            with self.assertRaisesRegex(ValueError, '本地没有'):
                release.main(['push'])
            before = git('rev-parse', 'HEAD')
            with patch('builtins.input', side_effect=['98.2.0', 'y', 'n']):
                release.release_wizard()
            commit = git('rev-parse', 'HEAD')
            self.assertNotEqual(before, commit)
            self.assertEqual(git('rev-parse', 'v98.2.0^{commit}'), commit)
            self.assertFalse(git('ls-remote', '--tags', 'origin'))
            self.assertIn(before + '\trefs/heads/main', git('ls-remote', 'origin', 'refs/heads/main'))
            with patch('builtins.input', return_value='n'):
                release.main(['push'])
            self.assertFalse(git('ls-remote', '--tags', 'origin'))
            with patch('builtins.input', return_value='') as prompt:
                release.main(['push'])
            self.assertEqual(prompt.call_count, 1)
            self.assertEqual(release.version(), '98.2.0')
            self.assertEqual(git('rev-parse', 'HEAD'), commit)
            self.assertIn(commit + '\trefs/tags/v98.2.0^{}', git('ls-remote', '--tags', 'origin'))

    def test_supported_release_aggregation_and_publish_reject_dirty_builds(self):
        source, manifest = self.package()
        incoming = self.root / 'incoming'
        commit = 'a' * 40
        for system, arch in [('linux', 'x86_64'), ('linux', 'aarch64'), ('darwin', 'aarch64')]:
            manifest.update(os=system, arch=arch, target=f'{arch}-unknown-linux-musl' if system == 'linux' else f'{arch}-apple-darwin', commit=commit, dirty=False)
            common.json_write(source / 'release.json', manifest)
            archive = self.archive(source)
            destination = incoming / f'release-{system}-{arch}'
            destination.mkdir(parents=True)
            name = f'chain-panel-0.1.0-{system}-{arch}.tar.gz'
            shutil.copy2(archive, destination / name)
            shutil.copy2(source / 'release.json', destination / 'release.json')
            (destination / 'SHA256SUMS').write_text(f'{common.sha256(archive)}  {name}\n')
        outgoing = self.root / 'published'
        with patch.object(ci, 'version', return_value='0.1.0'):
            ci.assemble(incoming, outgoing, 'v0.1.0', commit)
        self.assertEqual(len(list(outgoing.iterdir())), 7)
        for archive in outgoing.glob('*.tar.gz'):
            common.verify_archive_checksum(archive)
        with patch.object(ci, 'version', return_value='0.1.0'), \
                patch.object(ci, 'output', side_effect=RuntimeError('release lookup')):
            with self.assertRaisesRegex(RuntimeError, 'release lookup'):
                ci.publish(outgoing, 'v0.1.0', 'owner/repo', commit)
        manifest['dirty'] = True
        common.json_write(source / 'release.json', manifest)
        archive = self.archive(source)
        destination = incoming / 'release-darwin-aarch64'
        name = 'chain-panel-0.1.0-darwin-aarch64.tar.gz'
        shutil.copy2(archive, destination / name)
        shutil.copy2(source / 'release.json', destination / 'release.json')
        (destination / 'SHA256SUMS').write_text(f'{common.sha256(archive)}  {name}\n')
        with patch.object(ci, 'version', return_value='0.1.0'), self.assertRaisesRegex(ValueError, '拒绝发布'):
            ci.assemble(incoming, self.root / 'rejected', 'v0.1.0', commit)
        self.assertFalse((self.root / 'rejected').exists())

    def test_workspace_stops_owned_process_and_releases_port(self):
        with socket.socket() as reserve:
            reserve.bind(('127.0.0.1', 0))
            port = reserve.getsockname()[1]
        with common.Workspace(self.root) as work:
            directory = work.root
            work.manifest['ports'] = [port]
            work.record()
            code = "import socket,time,pathlib; s=socket.socket(); s.bind(('127.0.0.1', %d)); s.listen(); pathlib.Path(%r).write_text('ready'); time.sleep(45)" % (port, str(work.root / 'ready'))
            process = subprocess.Popen([sys.executable, '-c', code], env=work.env, start_new_session=True)
            work.children.append(process)
            work.manifest['groups'].append(process.pid)
            work.record()
            deadline = time.monotonic() + 5
            while not (work.root / 'ready').exists():
                self.assertLess(time.monotonic(), deadline)
                time.sleep(.02)
        self.assertIsNotNone(process.poll())
        with self.assertRaises(ProcessLookupError):
            os.killpg(process.pid, 0)
        self.assertFalse(directory.exists())
        with socket.socket() as check:
            check.bind(('127.0.0.1', port))
        print(f'CLEANUP verification PGID={process.pid}; port={port} released; directory removed.')


if __name__ == '__main__':
    unittest.main(verbosity=2)
