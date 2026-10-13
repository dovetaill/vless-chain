#!/usr/bin/env python3
"""Exercise SSH transfer against a private sshd and a harmless installer fixture."""
import contextlib
import hashlib
import json
import os
from pathlib import Path
import signal
import socket
import subprocess
import tempfile
import time


def main():
    if os.geteuid() != 0 or not Path('/usr/sbin/sshd').exists():
        raise SystemExit('SSH 集成测试需要 root 和 sshd；不会修改系统 SSH 配置。')
    with tempfile.TemporaryDirectory(prefix='chain-ssh-test-', dir=os.environ['CHAIN_TEST_ROOT']) as directory:
        root = Path(directory)
        (root / 'data').mkdir()
        release = root / 'release'
        (release / 'scripts').mkdir(parents=True)
        entry = release / 'scripts/install-server.sh'
        entry.write_text("#!/usr/bin/env bash\nset -eu\npython3 - \"$@\" <<'FIXTURE'\n"
                         "import pathlib,sys\nargs=sys.argv[1:]\n"
                         "assert args[:3]==['install','--component','agent']\n"
                         "assert '--non-interactive' in args\n"
                         "token=pathlib.Path(args[args.index('--token-file')+1])\n"
                         "assert token.read_text()=='s'*64\nassert token.stat().st_mode & 0o077 == 0\n"
                         f"pathlib.Path({str(root / 'accepted')!r}).write_text('ok')\n"
                         f"pathlib.Path({str(root / 'remote-path')!r}).write_text(str(token.parent))\n"
                         "print('fixture installer accepted private credentials')\nFIXTURE\n")
        manifest = {'format': 1, 'version': '0.1.0', 'files': {'scripts/install-server.sh': {'sha256': hashlib.sha256(entry.read_bytes()).hexdigest(), 'mode': 0o755}}}
        (release / 'release.json').write_text(json.dumps(manifest))
        for name in ('hostkey', 'identity'):
            subprocess.run(['ssh-keygen', '-q', '-t', 'ed25519', '-N', '', '-f', str(root / name)], check=True)
        (root / 'authorized_keys').write_bytes((root / 'identity.pub').read_bytes())
        (root / 'authorized_keys').chmod(0o600)
        with socket.socket() as reserved:
            reserved.bind(('127.0.0.1', 0))
            port = reserved.getsockname()[1]
        (root / 'port').write_text(str(port))
        config = root / 'sshd_config'
        config.write_text(f'Port {port}\nListenAddress 127.0.0.1\nHostKey {root}/hostkey\nPidFile {root}/sshd.pid\n'
                          f'AuthorizedKeysFile {root}/authorized_keys\nStrictModes yes\nPasswordAuthentication no\n'
                          'KbdInteractiveAuthentication no\nPermitRootLogin prohibit-password\nUsePAM no\n'
                          'AllowUsers root\nAllowTcpForwarding no\nX11Forwarding no\nLogLevel ERROR\n')
        log = (root / 'sshd.log').open('wb')
        record = {'cwd': str(root), 'port': port, 'root': str(root), 'pid': None}
        (root / 'ownership.json').write_text(json.dumps(record))
        daemon = subprocess.Popen(['/usr/sbin/sshd', '-D', '-e', '-f', str(config)], cwd=root,
                                  stdout=log, stderr=subprocess.STDOUT, start_new_session=True)
        record['pid'] = daemon.pid
        (root / 'ownership.json').write_text(json.dumps(record))
        print(f'SSH fixture PID/PGID={daemon.pid}; port={port}; root={root}', flush=True)
        try:
            deadline = time.monotonic() + 8
            while True:
                assert daemon.poll() is None, (root / 'sshd.log').read_text()
                try:
                    with socket.create_connection(('127.0.0.1', port), timeout=.2):
                        break
                except OSError:
                    assert time.monotonic() < deadline
                    time.sleep(.1)
            env = {**os.environ, 'CHAIN_TEST_SSH_ROOT': str(root), 'CHAIN_RELEASE_DIR': str(release), 'CHAIN_DATA_DIR': str(root / 'data')}
            subprocess.run(['cargo', 'test', '--locked', '--lib', 'real_ssh_transfer_and_host_identity', '--', '--ignored', '--nocapture'], env=env, check=True)
            remote = Path((root / 'remote-path').read_text())
            assert not remote.exists(), f'remote temporary directory remains: {remote}'
        finally:
            with contextlib.suppress(ProcessLookupError):
                os.killpg(daemon.pid, signal.SIGTERM)
            try:
                daemon.wait(timeout=5)
            except subprocess.TimeoutExpired:
                os.killpg(daemon.pid, signal.SIGKILL)
                daemon.wait(timeout=5)
            log.close()
            with socket.socket() as check:
                check.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
                check.bind(('127.0.0.1', port))
            print(f'CLEANUP sshd PGID={daemon.pid}; port={port} released; fixture files removed on exit.', flush=True)
    assert not root.exists()


if __name__ == '__main__':
    signal.signal(signal.SIGTERM, lambda *_: (_ for _ in ()).throw(KeyboardInterrupt()))
    main()
