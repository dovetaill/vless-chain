"""Run the native CLI against private offline installation trees; never start services."""
import hashlib
import json
import os
import pty
import select
import signal
import termios
import time
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
        units = self.root / ('Library/LaunchDaemons' if sys.platform == 'darwin' else 'etc/systemd/system')
        units.mkdir(parents=True)
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
            unit = units / (f'org.vless-chain.chain-{role}.plist' if sys.platform == 'darwin' else f'chain-{role}.service')
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
        (stage / 'release.json').write_text(json.dumps({'format': 1, 'version': '0.3.0', 'os': 'darwin' if sys.platform == 'darwin' else 'linux',
                                                      'arch': {'arm64': 'aarch64'}.get(os.uname().machine, os.uname().machine), 'files': files}))
        shutil.copytree(stage, self.base / 'releases/old', dirs_exist_ok=True)
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

    def terminal(self, args, marker, keys):
        pid, fd = pty.fork()
        if pid == 0:
            os.execv(self.binary, [str(self.binary), *args, '--root', str(self.root)])
        done, body = False, b''
        try:
            deadline = time.monotonic() + 15
            sent = False
            while True:
                if select.select([fd], [], [], .05)[0]:
                    try:
                        body += os.read(fd, 65536)
                    except OSError:
                        pass
                if not sent and marker in body:
                    os.write(fd, keys); sent = True
                waited, status = os.waitpid(pid, os.WNOHANG)
                if waited:
                    done = True
                    self.assertTrue(termios.tcgetattr(fd)[3] & termios.ECHO, 'terminal echo was not restored')
                    return os.waitstatus_to_exitcode(status), body.decode(errors='replace')
                self.assertLess(time.monotonic(), deadline, body)
        finally:
            if not done:
                os.killpg(pid, signal.SIGKILL); os.waitpid(pid, 0)
            os.close(fd)

    def test_menu_returns_after_status_and_shows_both_roles(self):
        code, body = self.terminal(['menu'], '选择'.encode(), b'4\n0\n')
        self.assertEqual(code, 0, body)
        self.assertIn('chain-panel', body)
        self.assertIn('chain-agent', body)
        self.assertGreaterEqual(body.count('Chain 原生管理'), 2)

    def test_connection_code_is_hidden_and_interrupt_restores_terminal(self):
        self.root = self.work / 'fresh'
        self.root.mkdir()
        code, body = self.terminal(['install', '--component', 'agent'], '粘贴'.encode(), b'PRIVATE_SENTINEL\x03')
        self.assertEqual(code, 130, body)
        self.assertNotIn('PRIVATE_SENTINEL', body)

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

    def test_packaged_install_uses_verified_local_bundle_without_download(self):
        binary = self.base / 'releases/old/bin/chainctl'
        binary.chmod(0o755)
        fresh = self.work / 'fresh'
        fresh.mkdir()
        result = subprocess.run([str(binary), 'install', '--root', str(fresh),
            '--panel-only', '--non-interactive'], stdin=subprocess.DEVNULL,
            capture_output=True, text=True, timeout=30)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(json.loads((fresh / 'opt/chain/.chain-install.json').read_text())['roles']['panel']['version'], '0.3.0')
        self.assertEqual(self.snapshot(), self.before)

    def test_legacy_wrapper_hands_off_to_verified_native_binary(self):
        self.update()
        wrapper = self.root / 'usr/local/bin/chainctl'
        wrapper.unlink()
        wrapper.write_text('#!/bin/sh\n# chain-panel-installer-v1\n')
        bridge = Path(__file__).resolve().parents[1] / 'legacy_bridge.py'
        result = subprocess.run([sys.executable, '-B', str(bridge), 'status', '--root', str(self.root)],
            capture_output=True, text=True, timeout=30)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue(wrapper.is_symlink())
        self.assertEqual(wrapper.resolve().name, 'chainctl')
        self.assertEqual(self.snapshot(), self.before)
        binary = (self.base / 'current-panel/bin/chainctl').resolve()
        binary.write_bytes(binary.read_bytes() + b'tamper')
        result = subprocess.run([sys.executable, '-B', str(bridge), 'status', '--root', str(self.root)],
            capture_output=True, text=True, timeout=30)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('校验失败', result.stderr)

    def test_uninstall_preserves_credentials_then_explicit_purge_removes_only_application_data(self):
        self.update()
        certificate = self.root / 'etc/letsencrypt/live/panel.example/fullchain.pem'
        certificate.parent.mkdir(parents=True)
        certificate.write_text('keep certificate')
        site = self.root / 'etc/nginx/conf.d/other.conf'
        site.parent.mkdir(parents=True)
        site.write_text('foreign site')
        original = {p: p.read_bytes() for p in self.protected if '.service' not in str(p) and '.plist' not in str(p)}
        self.cli('uninstall', '--non-interactive')
        state = json.loads(self.state_file.read_text())
        self.assertEqual(state['roles'], {})
        self.assertEqual(set(state['retained_roles']), {'panel', 'agent'})
        for path, data in original.items():
            self.assertEqual(path.read_bytes(), data)
        self.assertFalse((self.root / 'usr/local/bin/chainctl').exists())
        self.cli('uninstall', '--purge', '--non-interactive')
        for role in ('panel', 'agent'):
            self.assertFalse((self.root / ('var/lib/chain-' + role)).exists())
            self.assertFalse((self.root / ('etc/chain-' + role)).exists())
        self.assertEqual(site.read_text(), 'foreign site')
        self.assertEqual(certificate.read_text(), 'keep certificate')

    def test_uninstall_refuses_foreign_site_before_stopping_or_removing_anything(self):
        self.update()
        site = self.root / ('opt/homebrew/etc/nginx/conf.d/chain-panel.conf' if sys.platform == 'darwin' else 'etc/nginx/conf.d/chain-panel.conf')
        site.parent.mkdir(parents=True)
        site.write_text('foreign site')
        state = self.state_file.read_bytes()
        result = self.cli('uninstall', '--purge', '--non-interactive', success=False)
        self.assertIn('归属', result.stderr)
        self.assertEqual(self.state_file.read_bytes(), state)
        self.assertEqual(self.snapshot(), self.before)
        self.assertEqual(site.read_text(), 'foreign site')

    def test_candidate_worker_cannot_write_or_change_permissions_outside_its_directory(self):
        sandbox = self.work / 'sandbox'
        sandbox.mkdir()
        protected = self.work / 'protected'
        protected.write_text('keep')
        before = protected.stat()
        code = """import os, socket, pathlib
p = pathlib.Path(os.environ['PROTECTED'])
for action in [lambda: p.write_text('bad'), lambda: os.chmod(p, 0o777),
               lambda: socket.create_connection(('127.0.0.1', 9), 1)]:
    try:
        action()
    except PermissionError:
        pass
    else:
        raise AssertionError('sandbox escaped')
pathlib.Path('allowed').write_text('ok')
"""
        request = {'args': [sys.executable, '-c', code],
                   'env': {'PROTECTED': str(protected), 'PATH': '/usr/bin:/bin', 'HOME': str(sandbox)}}
        (sandbox / 'request.json').write_text(json.dumps(request))
        result = subprocess.run([str(self.binary), 'internal-core-worker', str(sandbox)],
            capture_output=True, text=True, timeout=15)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual((sandbox / 'allowed').read_text(), 'ok')
        after = protected.stat()
        self.assertEqual(protected.read_text(), 'keep')
        self.assertEqual((before.st_uid,before.st_gid,before.st_mode,before.st_ctime_ns),
                         (after.st_uid,after.st_gid,after.st_mode,after.st_ctime_ns))

    def test_checksum_failure_never_changes_current_installation(self):
        self.checksum.write_text('0' * 64 + '  native.tar.gz\n')
        result = self.update(success=False)
        self.assertIn('SHA256', result.stderr)
        self.assertEqual(json.loads(self.state_file.read_text()), self.state)
        self.assertEqual(self.snapshot(), self.before)
        self.assertEqual(sorted(p.name for p in (self.base / 'releases').iterdir()), ['old'])

    @unittest.skipUnless(sys.platform == 'linux', 'Linux seccomp check')
    def test_nginx_validation_acknowledges_bind_without_listening(self):
        sandbox = self.work / 'nginx-check'
        sandbox.mkdir()
        code = """import socket
s = socket.socket()
s.bind(('127.0.0.1', 0)); s.listen()
assert s.getsockname()[1] == 0
assert s.getsockopt(socket.SOL_SOCKET, socket.SO_ACCEPTCONN) == 0
try:
    socket.socket(type=socket.SOCK_DGRAM).sendto(b'blocked', ('127.0.0.1', 9))
except PermissionError:
    pass
else:
    raise AssertionError('network escaped')
"""
        (sandbox / 'request.json').write_text(json.dumps({'nginx_check': True,
            'args': [sys.executable, '-c', code], 'env': {'PATH': '/usr/bin:/bin'}}))
        result = subprocess.run([str(self.binary), 'internal-core-worker', str(sandbox)],
            capture_output=True, text=True, timeout=15)
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_foreign_service_and_interrupted_upgrade_are_not_overwritten(self):
        journal = self.base / '.update-pending.json'
        journal.write_text('{"fixture":"interrupted"}')
        self.assertIn('上次升级中断', self.update(success=False).stderr)
        self.assertEqual(journal.read_text(), '{"fixture":"interrupted"}')
        journal.unlink()
        unit = self.root / ('Library/LaunchDaemons/org.vless-chain.chain-agent.plist' if sys.platform == 'darwin' else 'etc/systemd/system/chain-agent.service')
        unit.write_text('foreign service')
        self.assertIn('归属', self.update(success=False).stderr)
        self.assertEqual(unit.read_text(), 'foreign service')
        self.assertEqual(json.loads(self.state_file.read_text()), self.state)
        self.assertEqual((self.base / 'current-panel').readlink(), Path('releases/old'))


if __name__ == '__main__':
    unittest.main()
