import concurrent.futures
import json
from pathlib import Path
import tempfile
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

    def run_job(self, action, before, after=None, exit_code=0, active=True):
        job = {'id': str(uuid.uuid4()), 'action': action, 'version': '1.2.4'}
        process = Mock(pid=123456, **{'wait.return_value': exit_code})
        with patch.object(maintenance, 'installed', side_effect=[before, after or before, after or before]), \
                patch.object(maintenance, 'download_release', return_value=(self.root / 'release.tar.gz', '1.2.4')), \
                patch.object(maintenance, 'safe_extract', return_value={'version': '1.2.4', 'files': ['scripts/chain_maintenance.py']}), \
                patch.object(maintenance.subprocess, 'Popen', return_value=process) as popen, \
                patch.object(maintenance.subprocess, 'run', return_value=Mock(returncode=0 if active else 1)) as run, \
                patch.object(maintenance.os, 'killpg'):
            maintenance.execute(job, self.root)
        self.assertEqual(list(self.root.glob('work-*')), [])
        result = json.loads((self.root / (job['id'] + '.json')).read_text())
        return result, [call.args[0] for call in popen.call_args_list], [call.args[0] for call in run.call_args_list]

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
