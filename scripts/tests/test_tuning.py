import concurrent.futures
import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
import uuid
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import chain_tuning as tuning
import chain_maintenance as maintenance


class TuningTests(unittest.TestCase):
    def setUp(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.root = Path(directory.name)
        self.values = dict(zip(tuning.KEYS, ('cubic', 'fq_codel')))
        self.calls = []
        patches = [patch.object(tuning, 'CONFIG', self.root / 'network.conf'),
                   patch.object(tuning, 'MODULE_CONFIG', self.root / 'modules.conf'),
                   patch.object(tuning, 'modules', return_value={'sch_fq', 'sch_fq_codel'}),
                   patch.object(tuning.shutil, 'which', return_value='/fixture/sysctl'),
                   patch.object(tuning, 'read_value', side_effect=self.read),
                   patch.object(tuning, 'command', side_effect=self.command)]
        for item in patches:
            item.start()
            self.addCleanup(item.stop)

    def read(self, key):
        return 'cubic bbr reno' if key.endswith('tcp_available_congestion_control') else self.values[key]

    def command(self, *args):
        self.calls.append(args)
        if args[0] == 'tc':
            return '[{"dev":"eth0","kind":"fq_codel","root":true}]'
        if args[0] == 'sysctl':
            key, value = args[2].split('=')
            self.values[key] = value
        return ''

    def test_detection_does_not_load_modules_or_write_defaults(self):
        state = tuning.status()
        self.assertIn('bbr', state['algorithms'])
        self.assertIn('fq', state['queues'])
        self.assertEqual(self.calls, [('tc', '-j', 'qdisc', 'show')])
        self.assertEqual(list(self.root.iterdir()), [])

    def test_apply_persists_only_two_defaults_and_reports_actual_queues(self):
        tuning.CONFIG.write_text(tuning.OWNER + '# old\n')
        before = tuning.CONFIG.stat()
        result = tuning.apply('bbr', 'fq')
        after = tuning.CONFIG.stat()
        self.assertEqual((before.st_ino, before.st_uid, before.st_gid, before.st_mode),
                         (after.st_ino, after.st_uid, after.st_gid, after.st_mode))
        self.assertEqual(tuning.CONFIG.read_text(), tuning.OWNER + 'net.ipv4.tcp_congestion_control = bbr\nnet.core.default_qdisc = fq\n')
        self.assertEqual(result['after']['qdisc'], 'fq')
        self.assertEqual(result['after']['interfaces'][0]['kind'], 'fq_codel')
        self.assertNotIn(('sysctl', '--system'), self.calls)
        self.assertFalse(any(c[0] == 'tc' and 'replace' in c for c in self.calls))

    def test_bad_or_unsupported_choices_have_no_mutations(self):
        for cc, qdisc in [('bbr;id', 'fq'), ('bbr', '../fq'), ('vegas', 'cake'), ('bbr', None)]:
            with self.assertRaises(ValueError):
                tuning.apply(cc, qdisc)
        self.assertFalse(any(c[0] != 'tc' for c in self.calls))

    def test_unowned_and_symlink_files_are_not_overwritten(self):
        tuning.CONFIG.write_text('# someone else\n')
        with self.assertRaisesRegex(ValueError, '不属于'):
            tuning.apply('bbr', 'fq')
        tuning.CONFIG.unlink()
        target = self.root / 'outside'
        target.write_text(tuning.OWNER)
        tuning.CONFIG.symlink_to(target)
        with self.assertRaisesRegex(ValueError, '类型'):
            tuning.apply('bbr', 'fq')
        self.assertEqual(target.read_text(), tuning.OWNER)
        self.assertFalse(any(c[0] != 'tc' for c in self.calls))

    def test_failed_sysctl_restores_previous_values(self):
        def fail(*args):
            if args[0] == 'sysctl' and args[2] == 'net.core.default_qdisc=fq':
                raise ValueError('denied')
            return self.command(*args)
        with patch.object(tuning, 'command', side_effect=fail):
            with self.assertRaisesRegex(ValueError, '已恢复'):
                tuning.apply('bbr', 'fq')
        self.assertEqual(tuple(self.values.values()), ('cubic', 'fq_codel'))
        self.assertEqual(list(self.root.iterdir()), [])

    def test_failed_persistence_restores_first_file_and_parameters(self):
        tuning.CONFIG.write_text(tuning.OWNER + '# previous\n')
        before = tuning.CONFIG.stat()
        replace = tuning.replace_owned
        def fail(path, previous, content):
            if path == tuning.MODULE_CONFIG:
                raise OSError('disk error')
            replace(path, previous, content)
        with patch.object(tuning, 'replace_owned', side_effect=fail):
            with self.assertRaisesRegex(ValueError, '已恢复'):
                tuning.apply('bbr', 'fq')
        self.assertEqual(tuning.CONFIG.read_text(), tuning.OWNER + '# previous\n')
        self.assertEqual(tuning.CONFIG.stat().st_ino, before.st_ino)
        self.assertEqual(tuple(self.values.values()), ('cubic', 'fq_codel'))

    def test_concurrent_sysctl_change_is_reported_and_not_overwritten(self):
        def fail(*args):
            if args[0] == 'sysctl' and args[2] == 'net.core.default_qdisc=fq':
                self.values[tuning.KEYS[0]] = 'reno'
                raise ValueError('denied')
            return self.command(*args)
        with patch.object(tuning, 'command', side_effect=fail):
            with self.assertRaisesRegex(ValueError, '恢复未完成'):
                tuning.apply('bbr', 'fq')
        self.assertEqual(self.values[tuning.KEYS[0]], 'reno')

    def test_maintenance_validates_and_preserves_tuning_receipts(self):
        request = {'id': str(uuid.uuid4()), 'action': 'network-tune', 'congestion_control': 'bbr', 'qdisc': 'fq'}
        maintenance.validate(request)
        with self.assertRaises(ValueError):
            maintenance.validate({**request, 'action': 'restart-all'})
        (self.root / (request['id'] + '.json')).write_text(json.dumps({**request, 'state': 'succeeded'}))
        with concurrent.futures.ThreadPoolExecutor(max_workers=1) as pool:
            self.assertEqual(maintenance.submit(request, pool, self.root)['state'], 'succeeded')
            with self.assertRaisesRegex(ValueError, '其他网络设置'):
                maintenance.submit({**request, 'qdisc': 'fq_codel'}, pool, self.root)
            state = maintenance.submit({'id': str(uuid.uuid4()), 'action': 'network-status'}, pool, self.root)
            self.assertIn('bbr', state['algorithms'])

    def test_executor_keeps_network_result_and_does_not_restart_services(self):
        job = {'id': str(uuid.uuid4()), 'action': 'network-tune', 'congestion_control': 'bbr', 'qdisc': 'fq'}
        with patch.object(maintenance, 'installed', return_value={'panel': '1.2.3'}), \
                patch.object(maintenance.subprocess, 'Popen') as process:
            maintenance.execute(job, self.root)
        self.assertEqual(job['state'], 'succeeded')
        self.assertEqual(job['network']['after']['congestion_control'], 'bbr')
        self.assertIn('默认队列已保存', job['detail'])
        process.assert_not_called()
        self.assertEqual(list(self.root.glob('work-*')), [])


if __name__ == '__main__':
    unittest.main()
