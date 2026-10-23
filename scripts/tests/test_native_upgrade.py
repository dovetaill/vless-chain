"""Run the native CLI against private offline installation trees; never start services."""
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tarfile
import tempfile
import unittest


class NativeUpgrade(unittest.TestCase):
    def setUp(self):
        self.assertNotEqual(os.geteuid(), 0, 'Run this simulation as a nonprivileged user')
        self.binary = Path(os.environ['CHAIN_TEST_CTL']).resolve(strict=True)
        temporary = tempfile.TemporaryDirectory(prefix='native-upgrade-', dir=os.environ['CHAIN_TEST_ROOT'])
        self.addCleanup(temporary.cleanup)
        self.work = Path(temporary.name)
        self.root = self.work / 'host'
        self.base = self.root / 'opt/chain'
        (self.base / 'releases/old').mkdir(parents=True)
        (self.root / 'var/tmp').mkdir(parents=True)
        (self.root / 'etc/systemd/system').mkdir(parents=True)
        self.state = {'owner': 'chain-panel-installer-v1', 'roles': {}}
        self.protected = []
        for role in ('panel', 'agent'):
            etc = self.root / ('etc/chain-' + role)
            data = self.root / ('var/lib/chain-' + role)
            etc.mkdir(parents=True)
            data.mkdir(parents=True)
            env = etc / (role + '.env')
            env.write_text('CHAIN_LISTEN="127.0.0.1:8088"\nCHAIN_NODE_ID="fixture-node"\n')
            env.chmod(0o600)
            token = etc / 'token'
            token.write_text('fixture-private-identity')
            token.chmod(0o600)
            database = data / 'traffic.db'
            database.write_bytes(b'fixture-business-data')
            self.protected.extend((env, token, database))
            unit = self.root / ('etc/systemd/system/chain-' + role + '.service')
            unit.write_text('# chain-panel-installer-v1\n')
            self.protected.append(unit)
            (self.base / ('current-' + role)).symlink_to('releases/old')
            self.state['roles'][role] = {'current': 'old', 'previous': None, 'version': '0.2.48', 'custom': 'preserve'}
        self.state_file = self.base / '.chain-install.json'
        self.state_file.write_text(json.dumps(self.state))
        self.state_file.chmod(0o600)
        self.before = self.snapshot()
        stage = self.work / 'stage'
        (stage / 'bin').mkdir(parents=True)
        shutil.copyfile(self.binary, stage / 'bin/chainctl')
        for name in ('chain-panel', 'chain-agent', 'xray'):
            (stage / 'bin' / name).write_text('offline fixture; must never execute')
        files = {str(p.relative_to(stage)): {'sha256': hashlib.sha256(p.read_bytes()).hexdigest(), 'mode': 0o755}
                 for p in (stage / 'bin').iterdir()}
        (stage / 'release.json').write_text(json.dumps({'format': 1, 'version': '0.3.0', 'os': 'linux',
                                                      'arch': os.uname().machine, 'files': files}))
        self.archive = self.work / 'native.tar.gz'
        with tarfile.open(self.archive, 'w:gz') as bundle:
            for path in stage.rglob('*'):
                if path.is_file():
                    bundle.add(path, arcname=str(path.relative_to(stage)), recursive=False)
        self.checksum = self.work / 'SHA256SUMS'
        self.checksum.write_text(hashlib.sha256(self.archive.read_bytes()).hexdigest() + '  native.tar.gz\n')

    def snapshot(self):
        result = {}
        for path in self.protected:
            info = path.stat()
            result[str(path)] = (path.read_bytes(), info.st_ino, info.st_uid, info.st_gid,
                                 info.st_mode, info.st_ctime_ns)
        return result

    def cli(self, *arguments, success=True):
        process = subprocess.run([str(self.binary), *arguments, '--root', str(self.root)],
                                 stdin=subprocess.DEVNULL, capture_output=True, text=True, timeout=30)
        self.assertEqual(process.returncode == 0, success, process.stdout + process.stderr)
        self.assertEqual(list((self.root / 'var/tmp').iterdir()), [], 'temporary extraction leaked')
        return process

    def update(self, *arguments, success=True):
        return self.cli('update', '--package', str(self.archive), '--non-interactive', *arguments, success=success)

    def test_real_cli_updates_both_roles_without_changing_credentials_or_data(self):
        self.update()
        state = json.loads(self.state_file.read_text())
        for role in ('panel', 'agent'):
            self.assertEqual(state['roles'][role]['version'], '0.3.0')
            self.assertEqual(state['roles'][role]['custom'], 'preserve')
            self.assertIsNone(state['roles'][role]['previous'])
            self.assertEqual((self.base / ('current-' + role)).resolve().name, state['roles'][role]['current'])
        self.assertEqual(self.snapshot(), self.before)
        self.assertFalse((self.root / 'var/backups').exists())
        self.assertFalse((self.base / '.update-pending.json').exists())
        self.assertIn('离线安装树', self.cli('status').stdout)

    def test_agent_only_update_preserves_panel_version(self):
        self.update('--component', 'agent')
        state = json.loads(self.state_file.read_text())
        self.assertEqual(state['roles']['agent']['version'], '0.3.0')
        self.assertEqual(state['roles']['panel'], self.state['roles']['panel'])
        self.assertEqual(self.snapshot(), self.before)

    def test_checksum_failure_never_changes_current_installation(self):
        self.checksum.write_text('0' * 64 + '  native.tar.gz\n')
        result = self.update(success=False)
        self.assertIn('SHA256', result.stderr)
        self.assertEqual(json.loads(self.state_file.read_text()), self.state)
        self.assertEqual(self.snapshot(), self.before)
        self.assertEqual(sorted(p.name for p in (self.base / 'releases').iterdir()), ['old'])

    def test_foreign_service_and_interrupted_upgrade_are_not_overwritten(self):
        journal = self.base / '.update-pending.json'
        journal.write_text('{"fixture":"interrupted"}')
        self.assertIn('上次升级中断', self.update(success=False).stderr)
        self.assertEqual(journal.read_text(), '{"fixture":"interrupted"}')
        journal.unlink()
        unit = self.root / 'etc/systemd/system/chain-agent.service'
        unit.write_text('foreign service')
        self.assertIn('归属', self.update(success=False).stderr)
        self.assertEqual(unit.read_text(), 'foreign service')
        self.assertEqual(json.loads(self.state_file.read_text()), self.state)
        self.assertEqual((self.base / 'current-panel').readlink(), Path('releases/old'))


if __name__ == '__main__':
    unittest.main()
