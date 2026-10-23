"""Time changes are mocked; these tests never invoke a system service."""
import sys
import subprocess
import contextlib
import io
import tempfile
from pathlib import Path
import unittest
from unittest.mock import patch, Mock
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import chain_time
import chain_install as installer
from chain_install import make_parser


class TimeDiagnostics(unittest.TestCase):
    def test_missing_optional_time_module_does_not_block_upgrade_download(self):
        with tempfile.TemporaryDirectory() as temporary:
            manager = Mock(live=True, root=Path(temporary))
            args = make_parser().parse_args(['update', '--non-interactive'])
            output = io.StringIO()
            with patch.dict(sys.modules, {'chain_time': None}), \
                    patch.object(installer, 'service_kind', return_value='systemd'), \
                    patch.object(installer, 'download_release', side_effect=ValueError('download reached')) as download, \
                    contextlib.redirect_stdout(output):
                with self.assertRaisesRegex(ValueError, 'download reached'):
                    installer.install_package(manager, args)
            download.assert_called_once()
            self.assertIn('时间同步未完成', output.getvalue())
            self.assertEqual(list((Path(temporary) / 'var/tmp').iterdir()), [])

    def test_install_and_update_enable_time_sync_without_prompting(self):
        for action in ('install', 'update'):
            for role in ('panel', 'agent'):
                for live, clock, failure, expected in (
                    (True, {'NTP': 'no', 'NTPSynchronized': 'no'}, None, True),
                    (True, {'NTP': 'no', 'NTPSynchronized': 'yes'}, None, True),
                    (True, {'NTP': 'yes', 'NTPSynchronized': 'no'}, None, True),
                    (True, {'NTP': 'yes', 'NTPSynchronized': 'yes'}, None, False),
                    (True, {}, ValueError('不支持的时间服务'), True),
                    (False, {}, None, False),
                    (True, subprocess.TimeoutExpired('timedatectl', 10), None, False),
                ):
                    with self.subTest(role=role, live=live, clock=clock), tempfile.TemporaryDirectory() as temporary:
                        base = Path(temporary)
                        (base / ('current-' + role)).mkdir()
                        manager = Mock(live=live, base=base)
                        manager.state.return_value = {'roles': {role: {}}}
                        manager.role_paths.return_value = (base, base, base)
                        args = make_parser().parse_args([action, '--component', role, '--panel-only', '--non-interactive'])
                        args.reuse_current = True
                        output = io.StringIO()
                        with patch.object(installer, 'verify_tree', return_value={}), \
                                patch.object(installer, 'compatible_release'), \
                                patch.object(installer, 'service_kind', return_value='systemd'), \
                                patch.object(chain_time, 'status', return_value=clock,
                                             side_effect=clock if isinstance(clock, Exception) else None) as status, \
                                patch.object(chain_time, 'synchronize', side_effect=failure) as sync, \
                                patch('builtins.input', side_effect=AssertionError('update must not prompt')), \
                                contextlib.redirect_stdout(output):
                            installer.install_package(manager, args)
                        self.assertEqual(status.call_count, int(live))
                        self.assertEqual(sync.call_count, int(expected))
                        manager.install.assert_called_once()
                        if failure or isinstance(clock, Exception):
                            self.assertIn('时间同步未完成', output.getvalue())

    def test_sync_preserves_timezone_and_uses_existing_daemon(self):
        clock = {'Timezone': 'America/New_York', 'NTP': 'no', 'NTPSynchronized': 'no'}
        with patch.object(chain_time, 'status', return_value=clock), \
                patch.object(chain_time.shutil, 'which', return_value='/usr/bin/chronyc'), \
                patch.object(chain_time.subprocess, 'run', return_value=Mock(stdout='chrony.service enabled', returncode=0)) as run:
            chain_time.synchronize()
        calls = [call.args[0] for call in run.call_args_list]
        self.assertIn(['systemctl', 'enable', '--now', 'chrony.service'], calls)
        self.assertIn(['chronyc', 'waitsync', '30', '0.1', '0', '2'], calls)
        self.assertFalse(any('apt-get' in call or 'set-timezone' in call or 'set-ntp' in call for call in calls))

    def test_missing_daemon_installs_chrony(self):
        with patch.object(chain_time, 'status', return_value={'Timezone': 'UTC'}), \
                patch.object(chain_time.shutil, 'which', return_value='/usr/bin/tool'), \
                patch.object(chain_time.subprocess, 'run', return_value=Mock(stdout='', returncode=0)) as run:
            chain_time.synchronize()
        calls = [call.args[0] for call in run.call_args_list]
        self.assertIn(['apt-get', 'install', '-y', 'chrony'], calls)
        for call in run.call_args_list:
            if call.args[0][0] == 'apt-get':
                self.assertEqual(call.kwargs['env']['DEBIAN_FRONTEND'], 'noninteractive')
        self.assertLess(calls.index(['chronyc', 'waitsync', '30', '0', '0', '2']), calls.index(['chronyc', 'makestep']))

    @unittest.skipUnless(chain_time.shutil.which('systemctl'), 'systemctl unavailable')
    def test_real_empty_unit_inventory_installs_chrony(self):
        run = subprocess.run
        with tempfile.TemporaryDirectory() as temporary:
            (Path(temporary) / 'etc/systemd/system').mkdir(parents=True)
            def command(args, **kwargs):
                if args[:2] == ['systemctl', 'list-unit-files']:
                    result = run([args[0], '--root', temporary, *args[1:]], **kwargs)
                    self.assertEqual(result.returncode, 1)
                    self.assertEqual((result.stdout, result.stderr), ('', ''))
                    return result
                return subprocess.CompletedProcess(args, 0, '', '')
            with patch.object(chain_time, 'status', return_value={'Timezone': 'America/New_York'}), \
                    patch.object(chain_time.shutil, 'which', return_value='/usr/bin/tool'), \
                    patch.object(chain_time.subprocess, 'run', side_effect=command) as calls:
                chain_time.synchronize()
            self.assertIn(['apt-get', 'install', '-y', 'chrony'], [call.args[0] for call in calls.call_args_list])

    def test_unit_inventory_failure_retains_stderr_and_does_not_install(self):
        output = io.StringIO()
        with patch.object(chain_time, 'status', return_value={'Timezone': 'UTC'}), \
                patch.object(chain_time.subprocess, 'run', return_value=subprocess.CompletedProcess(
                    ['systemctl', 'list-unit-files'], 1, '', 'Failed to list unit files: Permission denied\n')) as run, \
                contextlib.redirect_stderr(output):
            with self.assertRaisesRegex(ValueError, 'list-unit-files'):
                chain_time.synchronize()
        self.assertEqual(run.call_count, 1)
        self.assertIn('Permission denied', output.getvalue())

    def test_chrony_without_source_never_steps_clock(self):
        def command(args, **kwargs):
            return Mock(stdout='chrony.service enabled', returncode=1 if args[:2] == ['chronyc', 'waitsync'] else 0)
        with patch.object(chain_time, 'status', return_value={'Timezone': 'UTC'}), \
                patch.object(chain_time.shutil, 'which', return_value='/usr/bin/tool'), \
                patch.object(chain_time.subprocess, 'run', side_effect=command) as run:
            with self.assertRaisesRegex(ValueError, '有效同步时间源'):
                chain_time.synchronize()
        self.assertNotIn(['chronyc', 'makestep'], [call.args[0] for call in run.call_args_list])

    def test_existing_active_timesyncd_is_preserved(self):
        def command(args, **kwargs):
            return Mock(stdout='chrony.service disabled\nsystemd-timesyncd.service enabled',
                        returncode=1 if args == ['systemctl', 'is-active', '--quiet', 'chrony.service'] else 0)
        with patch.object(chain_time, 'status', return_value={'Timezone': 'UTC', 'NTPSynchronized': 'yes'}), \
                patch.object(chain_time.subprocess, 'run', side_effect=command) as run:
            chain_time.synchronize()
        calls = [call.args[0] for call in run.call_args_list]
        self.assertIn(['systemctl', 'enable', '--now', 'systemd-timesyncd.service'], calls)
        self.assertFalse(any(call[0] in ('chronyc', 'apt-get') for call in calls))

    def test_chrony_alias_is_not_a_second_service(self):
        with patch.object(chain_time, 'status', return_value={'Timezone': 'UTC'}), \
                patch.object(chain_time.shutil, 'which', return_value='/usr/bin/chronyc'), \
                patch.object(chain_time.subprocess, 'run', return_value=Mock(
                    stdout='chrony.service enabled\nchronyd.service alias', returncode=0)) as run:
            chain_time.synchronize()
        calls = [call.args[0] for call in run.call_args_list]
        self.assertNotIn(['systemctl', 'is-active', '--quiet', 'chronyd.service'], calls)

    def test_conflicting_active_daemons_do_not_change_clock(self):
        with patch.object(chain_time, 'status', return_value={'Timezone': 'UTC'}), \
                patch.object(chain_time.subprocess, 'run', return_value=Mock(
                    stdout='chrony.service enabled\nsystemd-timesyncd.service enabled', returncode=0)) as run:
            with self.assertRaisesRegex(ValueError, '多个运行中的时间服务'):
                chain_time.synchronize()
        self.assertFalse(any('enable' in call.args[0] for call in run.call_args_list))

    def test_sync_timeout_does_not_claim_success(self):
        with patch.object(chain_time, 'status', return_value={'Timezone': 'UTC', 'NTPSynchronized': 'no'}), \
                patch.object(chain_time.subprocess, 'run', return_value=Mock(stdout='systemd-timesyncd.service enabled', returncode=0)), \
                patch.object(chain_time.time, 'sleep'):
            with self.assertRaisesRegex(ValueError, '尚未确认同步'):
                chain_time.synchronize()

    def test_package_failure_names_failed_step(self):
        def command(args, **kwargs):
            if args[0] == 'apt-get':
                raise subprocess.CalledProcessError(100, args)
            return Mock(stdout='', returncode=0)
        with patch.object(chain_time, 'status', return_value={'Timezone': 'UTC'}), \
                patch.object(chain_time.shutil, 'which', return_value='/usr/bin/tool'), \
                patch.object(chain_time.subprocess, 'run', side_effect=command):
            with self.assertRaisesRegex(ValueError, 'apt-get update.*100'):
                chain_time.synchronize()

    def test_time_sync_precedes_download(self):
        with tempfile.TemporaryDirectory() as temporary:
            manager = Mock(live=True, root=Path(temporary))
            args = make_parser().parse_args(['install', '--component', 'agent', '--latest', '--non-interactive'])
            calls = []
            def download(*args):
                calls.append('download')
                raise ValueError('stop before download')
            with patch.object(installer, 'service_kind', return_value='systemd'), \
                    patch.object(chain_time, 'status', return_value={}), \
                    patch.object(chain_time, 'synchronize', side_effect=lambda: calls.append('sync')), \
                    patch.object(installer, 'download_release', side_effect=download):
                with self.assertRaisesRegex(ValueError, 'stop before download'):
                    installer.install_package(manager, args)
            self.assertEqual(calls, ['sync', 'download'])

    def test_uninstalled_update_does_not_change_time(self):
        with tempfile.TemporaryDirectory() as temporary:
            manager = installer.Manager(Path(temporary))
            args = make_parser().parse_args(['update'])
            with patch.object(chain_time, 'status') as status:
                with self.assertRaisesRegex(ValueError, '尚未安装'):
                    installer.install_package(manager, args)
            status.assert_not_called()

    def test_cli_follow_explicit_source(self):
        args = make_parser().parse_args(['logs', '--component', 'agent', '--log-source', 'xray', '--follow'])
        self.assertTrue(args.follow)
        self.assertEqual(args.log_source, 'xray')

if __name__ == '__main__':
    unittest.main()
