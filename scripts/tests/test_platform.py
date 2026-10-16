"""Platform decisions and service files, without changing host accounts or services."""
import io
import os
from pathlib import Path
import plistlib
import subprocess
import sys
import tarfile
import tempfile
import unittest
from types import SimpleNamespace
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import chain_common as common
import chain_install as installer
import chain_network as network
import chain_platform as platform
import chain_release as release


class Platforms(unittest.TestCase):
    def test_startup_failure_points_to_logs_without_requiring_chainctl(self):
        manager = installer.Manager.__new__(installer.Manager)
        manager.live, manager.root = True, Path('/')
        manager.system = SimpleNamespace(active=lambda role: False)
        for kind, expected in [('openrc', 'tail -n 100 /var/lib/chain-panel/service.log'),
                               ('launchd', 'tail -n 100 /var/log/chain-panel.log'),
                               ('systemd', 'journalctl -u chain-panel -n 100 --no-pager')]:
            with self.subTest(kind=kind), patch.object(installer, 'service_kind', return_value=kind), \
                    patch.object(installer.time, 'sleep'):
                with self.assertRaises(ValueError) as raised:
                    manager.healthy('panel', {'CHAIN_LISTEN': '127.0.0.1:37243'})
                self.assertIn(expected, str(raised.exception))
                self.assertNotIn('chainctl', str(raised.exception))

    def test_openrc_restart_and_rollback_recover_only_panel_and_agent(self):
        for name in ['chain-panel', 'chain-agent']:
            for action in ['stop', 'restart']:
                events = []
                with self.subTest(name=name, action=action), \
                        patch.object(platform, 'service_kind', return_value='openrc'), \
                        patch.object(platform, 'run', side_effect=lambda argv, **kw:
                                     (events.append(argv) or subprocess.CompletedProcess(argv, 0))), \
                        patch.object(platform, 'stop_openrc_residue', side_effect=lambda name: events.append(['recover', name])):
                    self.assertEqual(platform.service(action, name + '.service'), 0)
                expected = [['rc-service', name, 'stop'], ['recover', name]]
                if action == 'restart':
                    expected.append(['rc-service', name, 'start'])
                self.assertEqual(events, expected)
        for name in ['nginx', 'xray-chain-example', 'chain-panel-https']:
            with self.subTest(name=name), patch.object(platform, 'service_kind', return_value='openrc'), \
                    patch.object(platform, 'run', return_value=subprocess.CompletedProcess([], 0)) as run, \
                    patch.object(platform, 'stop_openrc_residue') as recover:
                platform.service('restart', name)
                run.assert_called_once_with(['rc-service', name, 'restart'], check=True)
                recover.assert_not_called()

    def test_openrc_restart_does_not_start_after_stop_failure(self):
        with patch.object(platform, 'service_kind', return_value='openrc'), \
                patch.object(platform, 'run', return_value=subprocess.CompletedProcess([], 1)) as run, \
                patch.object(platform, 'stop_openrc_residue') as recover:
            self.assertEqual(platform.service('restart', 'chain-panel', check=False), 1)
            run.assert_called_once_with(['rc-service', 'chain-panel', 'stop'], check=False)
            recover.assert_not_called()

    def test_remove_service_accounts_checks_ownership_and_platform_commands(self):
        user = SimpleNamespace(pw_name='chain-agent', pw_uid=987, pw_gid=987,
                               pw_dir='/var/lib/chain-agent', pw_shell='/usr/sbin/nologin')
        group = SimpleNamespace(gr_name='chain-agent', gr_mem=[])
        for os_name, binary, expected in [
                ('linux', True, [['userdel', 'chain-agent'], ['groupdel', 'chain-agent']]),
                ('linux', False, [['deluser', 'chain-agent'], ['delgroup', 'chain-agent']]),
                ('darwin', True, [['dscl', '.', '-delete', '/Users/chain-agent'], ['dscl', '.', '-delete', '/Groups/chain-agent']])]:
            with self.subTest(os=os_name, binary=binary), \
                    patch.object(platform.pwd, 'getpwnam', return_value=user), \
                    patch.object(platform.pwd, 'getpwall', return_value=[user]), \
                    patch.object(platform.grp, 'getgrgid', return_value=group), \
                    patch.object(platform.grp, 'getgrnam', return_value=group), \
                    patch.object(platform, 'operating_system', return_value=os_name), \
                    patch.object(platform.shutil, 'which', return_value='/bin/tool' if binary else None), \
                    patch.object(platform, 'run') as run:
                platform.remove_account('chain-agent')
                self.assertEqual([c.args[0] for c in run.call_args_list], expected)
                run.reset_mock()
                with patch.object(user, 'pw_uid', 0):
                    with self.assertRaisesRegex(ValueError, '归属不匹配'):
                        platform.remove_account('chain-agent')
                run.assert_not_called()

    def test_os_architecture_and_pinned_assets(self):
        for os_name, system in [('Linux', 'linux'), ('Darwin', 'darwin')]:
            for machine, arch in [('x86_64', 'x86_64'), ('arm64', 'aarch64'), ('aarch64', 'aarch64')]:
                with patch.object(common.platform, 'system', return_value=os_name), \
                        patch.object(common.platform, 'machine', return_value=machine):
                    self.assertEqual((common.operating_system(), common.architecture()), (system, arch))
                    self.assertIn((system, arch), release.XRAY_ASSETS)
                    self.assertIn((system, arch), release.MIHOMO_ASSETS)
        with patch.object(common.platform, 'system', return_value='Windows'):
            with self.assertRaisesRegex(ValueError, 'Linux 和 macOS'):
                common.architecture()

    def test_compatibility_rejects_wrong_os_and_old_macos_before_install(self):
        manifest = {'arch': 'aarch64', 'os': 'darwin', 'macos_min': '14.0'}
        with patch.object(common, 'architecture', return_value='aarch64'), \
                patch.object(common, 'operating_system', return_value='darwin'), \
                patch.object(common.platform, 'mac_ver', return_value=('14.0', (), '')):
            common.compatible_release(manifest)
            with self.assertRaisesRegex(ValueError, '操作系统'):
                common.compatible_release({**manifest, 'os': 'linux'})
            with patch.object(common.platform, 'mac_ver', return_value=('13.6', (), '')):
                with self.assertRaisesRegex(ValueError, 'macOS 14'):
                    common.compatible_release(manifest)
        with patch.object(common, 'architecture', return_value='aarch64'), \
                patch.object(common, 'operating_system', return_value='linux'), patch.object(common, 'run') as run:
            common.compatible_release({'arch': 'aarch64', 'os': 'linux', 'libc': 'musl'})
            run.assert_not_called()
            run.return_value = subprocess.CompletedProcess([], 1, '', '')
            with self.assertRaisesRegex(ValueError, 'glibc'):
                common.compatible_release({'arch': 'aarch64', 'glibc_min': '2.35'})

    def test_launchd_definition_keeps_user_env_and_process_cleanup(self):
        with patch.object(platform, 'service_kind', return_value='launchd'), patch.object(platform, 'output', return_value='10240'):
            value = plistlib.loads(platform.service_definition('chain-agent', ['/opt/chain/bin/agent'],
                    installer.OWNER, 'chain-agent', {'CHAIN_PANEL_URL': 'https://a/?x=$()&y="'}).encode())
            self.assertEqual(value['UserName'], 'chain-agent')
            self.assertEqual(value['EnvironmentVariables']['CHAIN_PANEL_URL'], 'https://a/?x=$()&y="')
            self.assertFalse(value['AbandonProcessGroup'])
            self.assertEqual(value['Umask'], 0o077)
            self.assertEqual(value['HardResourceLimits']['NumberOfFiles'], 10240)
            self.assertEqual(value['KeepAlive'], {'SuccessfulExit': False})
            renewal = plistlib.loads(platform.service_definition('renew', ['/bin/true'], 'owner', interval=43200).encode())
            self.assertEqual(renewal['StartInterval'], 43200)
            self.assertNotIn('KeepAlive', renewal)

    def test_openrc_definition_quotes_environment_without_evaluation(self):
        with patch.object(platform, 'service_kind', return_value='openrc'):
            value = platform.service_definition('chain-agent', ['/opt/chain/bin/agent'], installer.OWNER,
                                                'chain-agent', {'CHAIN_VALUE': '$(exit 77)"\'中文'})
        self.assertIn('capabilities="^cap_net_bind_service"', value)
        self.assertIn('supervisor=supervise-daemon', value)
        self.assertIn('stopgroup=yes', value)
        exports = '\n'.join(line for line in value.splitlines() if line.startswith('export '))
        result = subprocess.run(['/bin/sh', '-c', exports + '\nprintf %s "$CHAIN_VALUE"'], check=True, capture_output=True, text=True)
        self.assertEqual(result.stdout, '$(exit 77)"\'中文')

    def test_openrc_definition_preserves_runtime_command_path(self):
        inherited = '/custom/openrc/sbin:/custom/openrc/bin:/bin'
        for configured in [platform.SERVICE_PATH, '/custom service/bin:$(exit 77)']:
            with self.subTest(path=configured), patch.object(platform, 'service_kind', return_value='openrc'):
                value = platform.service_definition('chain-panel', ['/bin/true'], installer.OWNER,
                                                    env={'PATH': configured})
                result = subprocess.run(['/bin/sh', '-c', value + '\nprintf %s "$PATH"'],
                                        env={'PATH': inherited}, check=True, capture_output=True, text=True)
                self.assertEqual(result.stdout, configured + ':' + inherited)

    def test_openrc_logs_use_the_service_data_directory(self):
        for role in ['panel', 'agent']:
            with self.subTest(role=role), patch.object(platform, 'service_kind', return_value='openrc'), \
                    patch.object(installer, 'service_kind', return_value='openrc'):
                name = 'chain-' + role
                log = '/var/lib/' + name + '/service.log'
                value = platform.service_definition(name, ['/bin/true'], installer.OWNER, name)
                self.assertIn('output_log=' + log + '\nerror_log=' + log, value)
                with tempfile.TemporaryDirectory() as temporary:
                    subprocess.run(['/bin/sh', '-c', value.replace('/var/lib/' + name, temporary)
                                    + '\nprintf log-check >> "$output_log"'], check=True)
                    self.assertEqual((Path(temporary) / 'service.log').read_text(), 'log-check')
                manager = installer.Manager.__new__(installer.Manager)
                manager.live, manager.root = True, Path('/')
                args = SimpleNamespace(action='logs', component=role)
                with patch.object(manager, 'require_role'), patch.object(installer, 'run') as run:
                    manager.operate(args)
                run.assert_called_once_with(['tail', '-n', '100', log])

    def test_launchd_restart_unloads_old_definition_before_bootstrap(self):
        with patch.object(platform, 'service_kind', return_value='launchd'), \
                patch.object(platform.subprocess, 'run') as run:
            run.side_effect = [subprocess.CompletedProcess([], 0), subprocess.CompletedProcess([], 0),
                               subprocess.CompletedProcess([], 1), subprocess.CompletedProcess([], 0)]
            platform.service('restart', 'chain-agent.service')
        self.assertEqual(run.call_args_list[1].args[0], ['launchctl', 'bootout', 'system/org.vless-chain.chain-agent'])
        self.assertEqual(run.call_args_list[-1].args[0], ['launchctl', 'bootstrap', 'system', '/Library/LaunchDaemons/org.vless-chain.chain-agent.plist'])

    def test_launchd_idle_timer_stays_active_for_rollback(self):
        with patch.object(platform, 'service_kind', return_value='launchd'), \
                patch.object(platform, 'output', return_value='disabled services = {}'), \
                patch.object(platform.subprocess, 'run', return_value=subprocess.CompletedProcess([], 0, 'state = waiting\n')):
            self.assertTrue(platform.service_state('chain-panel-cert-renew.timer')[0])
            self.assertFalse(platform.service_state('chain-agent')[0])

    def test_alpine_and_homebrew_packages(self):
        with patch.object(network, 'operating_system', return_value='linux'), \
                patch.object(network.shutil, 'which', side_effect=lambda name: '/sbin/apk' if name == 'apk' else None), \
                patch.object(network, 'run') as run:
            network.install_packages(['iproute', 'libnginx-mod-stream'])
            self.assertEqual(run.call_args.args[0], ['apk', 'add', '--no-cache', 'iproute2', 'nginx-mod-stream'])
        with patch.dict(os.environ, {'SUDO_USER': 'alice'}), \
                patch.object(platform.shutil, 'which', return_value='/opt/homebrew/bin/brew'):
            self.assertEqual(platform.brew_command('install', 'python'),
                             ['sudo', '-H', '-u', 'alice', '/opt/homebrew/bin/brew', 'install', 'python'])

    def test_macos_address_detection_uses_only_up_global_interfaces(self):
        interfaces = ('en0: flags=8863<UP,BROADCAST,RUNNING>\n\tinet 8.8.8.8 netmask 0xffffff00\n'
                      'en1: flags=2<BROADCAST>\n\tinet 1.1.1.1 netmask 0xffffff00\n'
                      'lo0: flags=8049<UP,LOOPBACK>\n\tinet 127.0.0.1 netmask 0xff000000\n')
        with patch.object(network, 'operating_system', return_value='darwin'), \
                patch.object(network, 'output', return_value=interfaces):
            self.assertEqual(network.server_addresses(), {'8.8.8.8'})

    def test_homebrew_relative_site_include(self):
        dump = '# configuration file /opt/homebrew/etc/nginx/nginx.conf:\nhttp { include servers/*; }\n'
        self.assertEqual(network.nginx_site_directory(dump), Path('/opt/homebrew/etc/nginx/servers'))

    def test_non_systemd_install_upgrade_rollback_and_uninstall(self):
        from test_delivery import Delivery
        for kind, system in [('openrc', 'Linux'), ('launchd', 'Darwin')]:
            fixture = Delivery()
            fixture.setUp()
            try:
                with patch.object(common.platform, 'system', return_value=system), \
                        patch.object(common.platform, 'mac_ver', return_value=('14.0', (), '')), \
                        patch.object(platform, 'service_kind', return_value=kind), \
                        patch.object(network, 'service_kind', return_value=kind), \
                        patch('chain_ingress.service_kind', return_value=kind), \
                        patch.object(installer, 'service_kind', return_value=kind), \
                        patch.object(installer, 'service_path', side_effect=lambda name: platform.service_path(name, kind)):
                    manager = fixture.manager
                    source, manifest = fixture.package()
                    manager.install(source, manifest, fixture.args(listen='127.0.0.1:8088'))
                    etc, data, unit = manager.role_paths('panel')
                    env = (etc / 'panel.env').read_bytes()
                    (data / 'keep').write_text('business data')
                    if kind == 'openrc':
                        (data / 'service.log').write_text('service output')
                    previous = manager.state()['roles']['panel']['current']
                    source, manifest = fixture.package('0.2.0')
                    manager.install(source, manifest, fixture.args(action='update'))
                    if kind == 'openrc':
                        backup = next(manager.path('/var/backups/chain-panel').glob('*.tar.gz'))
                        with tarfile.open(backup) as archive:
                            self.assertNotIn('data/service.log', archive.getnames())
                    self.assertEqual((etc / 'panel.env').read_bytes(), env)
                    if kind == 'launchd':
                        service = plistlib.loads(unit.read_bytes())
                        self.assertEqual(service['EnvironmentVariables']['CHAIN_LISTEN'], '127.0.0.1:8088')
                        self.assertEqual(service['UserName'], 'chain-panel')
                    else:
                        self.assertEqual(unit.stat().st_mode & 0o777, 0o755)
                        subprocess.run(['/bin/sh', '-n', str(unit)], check=True)
                    manager.rollback('panel')
                    self.assertEqual(manager.state()['roles']['panel']['current'], previous)
                    self.assertEqual((data / 'keep').read_text(), 'business data')
                    args = installer.make_parser().parse_args(['uninstall', '--non-interactive'])
                    manager.operate(args)
                    self.assertFalse((data / 'service.log').exists())
                    self.assertFalse(unit.exists())
                    self.assertEqual((data / 'keep').read_text(), 'business data')
                    args.purge = True
                    manager.operate(args)
                    self.assertFalse(data.exists())
                    self.assertFalse(manager.base.exists())
            finally:
                fixture.tearDown()
                fixture.doCleanups()

    def test_non_systemd_renewal_keeps_certificate_scope(self):
        for kind in ['openrc', 'launchd']:
            with tempfile.TemporaryDirectory(prefix='chain-platform-renew-') as temporary, \
                    patch.object(network, 'service_kind', return_value=kind), \
                    patch.object(platform, 'service_kind', return_value=kind), \
                    patch.object(network, 'service') as service, \
                    patch.object(network.shutil, 'which', side_effect=lambda name: '/usr/local/bin/' + name):
                root = Path(temporary)
                paths = root / 'hook', root / 'renew.sh', root / 'service'
                site = network.HttpsSite('panel.example.com', 8088)
                with patch.object(site, 'renewal_paths', return_value=paths):
                    site.renewal()
                self.assertIn('--cert-name chain-panel-panel.example.com', paths[1].read_text())
                self.assertIn('--config-dir /etc/letsencrypt', paths[1].read_text())
                service.assert_any_call('restart', 'chain-panel-cert-renew')
                if kind == 'launchd':
                    self.assertEqual(plistlib.loads(paths[2].read_bytes())['StartInterval'], 43200)
                else:
                    subprocess.run(['/bin/sh', '-n', str(paths[2])], check=True)

    def test_posix_bootstrap_help_from_an_empty_directory(self):
        script = Path(__file__).resolve().parents[1] / 'install-server.sh'
        with tempfile.TemporaryDirectory(prefix='chain-bootstrap-test-') as temporary:
            result = subprocess.run(['/bin/sh', '-s', '--', 'help'], input=script.read_text(), cwd=temporary,
                                    capture_output=True, text=True, check=True)
            self.assertIn('Linux', result.stdout)
            self.assertIn('macOS', result.stdout)
            self.assertEqual(list(Path(temporary).iterdir()), [])

    def test_bootstrap_downloads_https_runtime_dependencies(self):
        script = Path(__file__).resolve().parents[1] / 'install-server.sh'
        bootstrap = script.read_text().split("<<'PY'\n", 1)[1].split('\nPY\n', 1)[0]

        def download(request, timeout):
            if isinstance(request, str):
                return io.BytesIO((script.parent / request.rsplit('/', 1)[-1]).read_bytes())
            return io.BytesIO(b'{"sha": "' + b'a' * 40 + b'"}')

        with tempfile.TemporaryDirectory(prefix='chain-bootstrap-test-') as temporary:
            with patch('urllib.request.urlopen', side_effect=download), \
                    patch.object(sys, 'argv', ['-', temporary]):
                exec(compile(bootstrap, str(script), 'exec'), {'__name__': '__main__'})
            result = subprocess.run(
                [sys.executable, '-B', '-S', '-c',
                 'import chain_install; from chain_ingress import refresh_panel_site'],
                cwd=temporary, env={**os.environ, 'PYTHONPATH': ''},
                capture_output=True, text=True, timeout=10)
            self.assertEqual(result.returncode, 0, result.stderr)
        self.assertFalse(Path(temporary).exists())


if __name__ == '__main__':
    unittest.main(verbosity=2)
