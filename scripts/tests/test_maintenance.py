import concurrent.futures
import json
from pathlib import Path
import tempfile
import subprocess
import unittest
from unittest.mock import Mock, patch
import uuid
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

import chain_maintenance as maintenance
from chain_install import Manager, OWNER


class MaintenanceTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        unit = patch.object(maintenance, 'HTTPS_UNIT', self.root / 'chain-panel-https.service')
        unit.start()
        self.addCleanup(unit.stop)
        self.addCleanup(maintenance.RELOAD.clear)

    def run_job(self, action, before, after=None, exit_code=0, active=True, output='', timeout=False, channel=None):
        job = {'id': str(uuid.uuid4()), 'action': action, 'version': '1.2.4'}
        if channel is not None:
            job['xray_channel'] = channel
        process = Mock(pid=123456, **{'wait.return_value': exit_code})
        if timeout:
            process.wait.side_effect = [subprocess.TimeoutExpired('fixture', 1800), 0, 0]

        def start(command, **kwargs):
            kwargs['stdout'].write(output.encode())
            return process

        with patch.object(maintenance, 'installed', side_effect=[before, after or before, after or before]), \
                patch.object(maintenance, 'download_release', return_value=(self.root / 'release.tar.gz', '1.2.4')), \
                patch.object(maintenance, 'safe_extract', return_value={'version': '1.2.4', 'files': ['scripts/chain_maintenance.py']}), \
                patch.object(maintenance.subprocess, 'Popen', side_effect=start) as popen, \
                patch.object(maintenance.subprocess, 'run', return_value=Mock(returncode=0 if active else 1)) as run, \
                patch.object(maintenance.os, 'killpg'):
            maintenance.execute(job, self.root)
        self.assertEqual(list(self.root.glob('work-*')), [])
        result = json.loads((self.root / (job['id'] + '.json')).read_text())
        return result, [call.args[0] for call in popen.call_args_list], [call.args[0] for call in run.call_args_list]

    def test_time_failure_retains_redacted_diagnostics_without_work_files(self):
        output = ('正在安装时间服务\nFailed to fetch https://user:pass@mirror.example/p?token=hidden\n'
                  'Authorization: Bearer bearer-secret\npassword=login-secret token=api-secret\n'
                  '"password": "quoted secret with spaces"\n'
                  'E: Unable to locate package chrony\n')
        result, _, _ = self.run_job('sync-time', {'panel': '1.2.3'}, exit_code=1, output=output)
        self.assertEqual(result['state'], 'failed')
        self.assertIn('正在安装时间服务', result['detail'])
        self.assertIn('Unable to locate package chrony', result['error'])
        for secret in ['user:pass', 'mirror.example', 'bearer-secret', 'login-secret', 'api-secret', 'quoted secret']:
            self.assertNotIn(secret, json.dumps(result))
        result, _, _ = self.run_job('upgrade', {'panel': '1.2.3'}, exit_code=1, output=output)
        self.assertNotIn('detail', result)
        self.assertNotIn('Unable to locate', result['error'])

    def test_time_output_is_bounded_and_timeout_keeps_last_stage(self):
        result, _, _ = self.run_job('sync-time', {'panel': '1.2.3'}, output='x\n' * 40000 + '等待 NTP 响应', timeout=True)
        self.assertEqual(result['state'], 'failed')
        self.assertIn('超时', result['error'])
        self.assertTrue(result['detail'].endswith('等待 NTP 响应'))
        self.assertLessEqual(len(result['detail']), 4096)
        result, _, _ = self.run_job('sync-time', {'panel': '1.2.3'}, exit_code=1,
                                    output='https://user:pass@' + 'secret' * 20000)
        self.assertEqual(result['detail'], '')
        self.assertNotIn('secret', result['error'])

    def test_time_success_retains_confirmation_and_timezone(self):
        result, commands, _ = self.run_job('sync-time', {'panel': '1.2.3'},
                                          output='时间同步已确认；时区保持 America/New_York\n')
        self.assertEqual(result['state'], 'succeeded')
        self.assertIn('America/New_York', result['detail'])
        self.assertEqual(len(commands), 1)
        self.assertTrue(commands[0][-1].endswith('chain_time.py'))

    def test_restart_all_includes_only_installed_project_services(self):
        maintenance.HTTPS_UNIT.write_text(maintenance.SITE_OWNER + '\n[Service]\n')
        result, commands, checks = self.run_job('restart-all', {'panel': '1.2.3', 'agent': '1.2.3'})
        services = ['chain-panel.service', 'chain-agent.service', 'chain-panel-https.service']
        self.assertEqual(result['state'], 'succeeded')
        self.assertEqual(commands, [['/usr/bin/systemctl', 'restart', *services]])
        self.assertEqual(checks, [['/usr/bin/systemctl', 'is-active', '--quiet', service] for service in services])
        maintenance.HTTPS_UNIT.unlink()
        result, commands, _ = self.run_job('restart-all', {'panel': '1.2.3'})
        self.assertEqual(commands, [['/usr/bin/systemctl', 'restart', 'chain-panel.service']])

    def test_foreign_https_service_is_never_restarted(self):
        maintenance.HTTPS_UNIT.write_text('foreign service')
        result, commands, _ = self.run_job('restart-all', {'panel': '1.2.3'})
        self.assertEqual(result['state'], 'failed')
        self.assertIn('归属不匹配', result['error'])
        self.assertEqual(commands, [])

    def test_upgrade_restarts_https_and_verifies_all_installed_versions(self):
        maintenance.HTTPS_UNIT.write_text(maintenance.SITE_OWNER + '\n[Service]\n')
        before = {'panel': '1.2.3', 'agent': '1.2.3'}
        result, commands, _ = self.run_job('upgrade', before, {'panel': '1.2.4', 'agent': '1.2.4'})
        self.assertEqual(result['state'], 'succeeded')
        self.assertEqual(commands[0][:4], ['/usr/local/bin/chainctl', 'update', '--component', 'panel'])
        self.assertEqual(commands[1], ['/usr/bin/systemctl', 'restart', 'chain-panel-https.service'])
        result, _, _ = self.run_job('upgrade', before, {'panel': '1.2.4', 'agent': '1.2.3'})
        self.assertEqual(result['state'], 'failed')
        self.assertIn('agent 版本未达到目标', result['error'])

    def test_latest_core_requires_explicit_choice_and_is_part_of_task_identity(self):
        result, commands, _ = self.run_job('upgrade', {'panel': '1.2.3'}, {'panel': '1.2.4'}, channel='latest')
        self.assertEqual(result['state'], 'succeeded')
        self.assertEqual(commands[0][-2:], ['--xray-channel', 'latest'])
        payload = {'id': str(uuid.uuid4()), 'action': 'upgrade', 'version': '1.2.4'}
        pool = Mock(spec=concurrent.futures.ThreadPoolExecutor)
        maintenance.submit(payload, pool, self.root)
        with self.assertRaisesRegex(ValueError, '其他操作'):
            maintenance.submit({**payload, 'xray_channel': 'latest'}, pool, self.root)
        for value in ({**payload, 'xray_channel': 'nightly'}, {**payload, 'action': 'restart-all', 'xray_channel': 'latest'}):
            with self.assertRaises(ValueError): maintenance.validate(value)

    def test_newer_agent_blocks_downgrade_and_failed_update_stops_sequence(self):
        result, commands, _ = self.run_job('upgrade', {'panel': '1.2.3', 'agent': '1.2.5'})
        self.assertEqual(result['state'], 'failed')
        self.assertEqual(commands, [])
        maintenance.HTTPS_UNIT.write_text(maintenance.SITE_OWNER + '\n[Service]\n')
        result, commands, checks = self.run_job('upgrade', {'panel': '1.2.3'}, exit_code=1)
        self.assertEqual(result['state'], 'failed')
        self.assertEqual(len(commands), 1)
        self.assertEqual(checks, [])
        result, _, _ = self.run_job('restart-all', {'panel': '1.2.3'}, active=False)
        self.assertEqual(result['state'], 'failed')
        self.assertIn('服务未运行', result['error'])

    def test_download_failure_records_result_and_removes_owned_work(self):
        with tempfile.TemporaryDirectory() as root:
            root = Path(root)
            job = {'id': str(uuid.uuid4()), 'action': 'upgrade', 'version': '1.2.4'}
            with patch.object(maintenance, 'installed', return_value={'panel': '1.2.3', 'agent': '1.2.3'}), \
                    patch.object(maintenance, 'download_release', side_effect=ValueError('fixture download failure')), \
                    patch.object(maintenance.subprocess, 'Popen') as process:
                maintenance.execute(job, root)
            result = json.loads((root / (job['id'] + '.json')).read_text())
            self.assertEqual(result['state'], 'failed')
            self.assertEqual(result['after'], {'panel': '1.2.3', 'agent': '1.2.3'})
            self.assertEqual(list(root.glob('work-*')), [])
            process.assert_not_called()
            maintenance.RELOAD.clear()

    def test_rejects_arbitrary_commands_paths_and_versions(self):
        for payload in [
            {'id': str(uuid.uuid4()), 'action': 'shell', 'command': 'id'},
            {'id': '../../outside', 'action': 'restart-panel'},
            {'id': str(uuid.uuid4()), 'action': 'upgrade', 'version': '1.2.3;id'},
            {'id': str(uuid.uuid4()), 'action': 'upgrade', 'version': '../latest'},
        ]:
            with self.assertRaises(ValueError):
                maintenance.validate(payload)

    def test_repeated_request_is_not_executed_again(self):
        with tempfile.TemporaryDirectory() as root:
            root = Path(root)
            pool = Mock(spec=concurrent.futures.ThreadPoolExecutor)
            payload = {'id': str(uuid.uuid4()), 'action': 'upgrade', 'version': '1.2.3'}
            first = maintenance.submit(payload, pool, root)
            again = maintenance.submit(payload, pool, root)
            self.assertEqual(first, again)
            pool.submit.assert_called_once()
            with self.assertRaises(ValueError):
                maintenance.submit({**payload, 'version': '1.2.4'}, pool, root)
            with self.assertRaises(ValueError):
                maintenance.submit({**payload, 'id': str(uuid.uuid4())}, pool, root)
            result = maintenance.submit({'action': 'status', 'id': payload['id']}, pool, root)
            self.assertEqual(result['state'], 'queued')

    def test_socket_is_restricted_and_foreign_unit_is_preserved(self):
        with tempfile.TemporaryDirectory() as root, patch('chain_install.service_kind', return_value='systemd'):
            manager = Manager(Path(root))
            script = manager.base / 'current-panel/scripts/chain_maintenance.py'
            script.parent.mkdir(parents=True)
            script.write_text('# placeholder')
            unit_dir = manager.path('/etc/systemd/system')
            unit_dir.mkdir(parents=True)
            manager.maintenance_units()
            socket = (unit_dir / 'chain-maintenance.socket').read_text()
            self.assertIn('SocketGroup=chain-panel', socket)
            self.assertIn('SocketMode=0660', socket)
            service = (unit_dir / 'chain-maintenance.service').read_text()
            self.assertIn('KillMode=control-group', service)
            self.assertIn(OWNER, service)
            foreign = unit_dir / 'chain-maintenance.socket'
            foreign.write_text('foreign-service')
            with self.assertRaises(ValueError):
                manager.maintenance_units()
            self.assertEqual(foreign.read_text(), 'foreign-service')


if __name__ == '__main__':
    unittest.main()
