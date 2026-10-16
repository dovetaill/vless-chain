"""Platform decisions and service files, without changing host accounts or services."""
import os
from pathlib import Path
import plistlib
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import chain_common as common
import chain_install as installer
import chain_network as network
import chain_platform as platform
import chain_release as release


class Platforms(unittest.TestCase):
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

    def test_non_systemd_install_upgrade_and_rollback(self):
        from test_delivery import Delivery
        for kind, system in [('openrc', 'Linux'), ('launchd', 'Darwin')]:
            fixture = Delivery()
            fixture.setUp()
            try:
                with patch.object(common.platform, 'system', return_value=system), \
                        patch.object(common.platform, 'mac_ver', return_value=('14.0', (), '')), \
                        patch.object(platform, 'service_kind', return_value=kind), \
                        patch.object(installer, 'service_kind', return_value=kind), \
                        patch.object(installer, 'service_path', side_effect=lambda name: platform.service_path(name, kind)):
                    manager = fixture.manager
                    source, manifest = fixture.package()
                    manager.install(source, manifest, fixture.args(listen='127.0.0.1:8088'))
                    etc, data, unit = manager.role_paths('panel')
                    env = (etc / 'panel.env').read_bytes()
                    (data / 'keep').write_text('business data')
                    previous = manager.state()['roles']['panel']['current']
                    source, manifest = fixture.package('0.2.0')
                    manager.install(source, manifest, fixture.args(action='update'))
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


if __name__ == '__main__':
    unittest.main(verbosity=2)
