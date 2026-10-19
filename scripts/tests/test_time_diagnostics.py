"""Time changes are mocked; these tests never invoke a system service."""
import sys
from pathlib import Path
import unittest
from unittest.mock import patch, Mock
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import chain_time
from chain_install import make_parser


class TimeDiagnostics(unittest.TestCase):
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
