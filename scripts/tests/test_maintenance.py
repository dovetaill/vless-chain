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
