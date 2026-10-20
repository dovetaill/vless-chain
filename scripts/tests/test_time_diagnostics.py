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
    def test_updates_enable_time_sync_without_prompting(self):
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
                    args = make_parser().parse_args(['update', '--component', role, '--non-interactive'])
                    args.reuse_current = True
                    output = io.StringIO()
                    with patch.object(installer, 'verify_tree', return_value={}), \
                            patch.object(installer, 'compatible_release'), \
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
        states = [{'Timezone': 'Asia/Hong_Kong', 'NTP': 'no', 'NTPSynchronized': 'no'},
                  {'Timezone': 'Asia/Hong_Kong', 'NTP': 'yes', 'NTPSynchronized': 'yes'}]
        with patch.object(chain_time, 'status', side_effect=states), patch.object(chain_time.subprocess, 'run', return_value=Mock(stdout='chrony.service enabled')) as run:
            chain_time.synchronize()
        calls = [call.args[0] for call in run.call_args_list]
        self.assertEqual(calls[-1], ['timedatectl', 'set-ntp', 'true'])
        self.assertFalse(any('apt-get' in call or 'set-timezone' in call for call in calls))

    def test_sync_timeout_does_not_claim_success(self):
        with patch.object(chain_time, 'status', return_value={'Timezone': 'UTC', 'NTPSynchronized': 'no'}), patch.object(chain_time.subprocess, 'run', return_value=Mock(stdout='systemd-timesyncd.service enabled')), patch.object(chain_time.time, 'sleep'):
            with self.assertRaisesRegex(ValueError, '尚未确认同步'):
                chain_time.synchronize()

    def test_cli_follow_explicit_source(self):
        args = make_parser().parse_args(['logs', '--component', 'agent', '--log-source', 'xray', '--follow'])
        self.assertTrue(args.follow)
        self.assertEqual(args.log_source, 'xray')

if __name__ == '__main__':
    unittest.main()
