#!/usr/bin/env python3
"""Offline service files plus real controller/Agent verify automatic local enrollment."""
import os
from pathlib import Path
import signal
import sys
import tempfile
import json
import subprocess
import hashlib
import shlex
import tarfile

sys.path.insert(0, str(Path(__file__).resolve().parents[2] / 'scripts'))
from chain_common import verify_tree
def parse_env(path):
    return dict((k, shlex.split(v)[0]) for k, v in (line.split('=', 1) for line in path.read_text().splitlines() if '=' in line))
from smoke import Harness


def main():
    source = Path(sys.argv[1]).resolve()
    manifest = verify_tree(source)
    os.environ['CHAIN_TEST_BIN_DIR'] = str(source / 'bin')
    os.environ['CHAIN_TEST_XRAY'] = str(source / 'bin/xray')
    with tempfile.TemporaryDirectory(prefix='chain-local-enroll-', dir=os.environ['CHAIN_TEST_ROOT']) as temporary:
        root = Path(temporary)
        harness = Harness(root)
        try:
            port = harness.port()
            harness.base = f'http://127.0.0.1:{port}'
            system = root / 'system'
            system.mkdir()
            archive = root / 'native.tar.gz'
            with tarfile.open(archive, 'w:gz') as bundle:
                for name in [*manifest['files'], 'release.json']:
                    bundle.add(source / name, arcname=name, recursive=False)
            (root / 'SHA256SUMS').write_text(hashlib.sha256(archive.read_bytes()).hexdigest() + '  native.tar.gz\n')
            command = [str(source / 'bin/chainctl'), 'install', '--root', str(system), '--package', str(archive),
                       '--work-parent', str(root), '--non-interactive', '--listen', f'127.0.0.1:{port}',
                       '--local-host', '127.0.0.1', '--local-target', 'localhost:443']
            def install(*extra):
                subprocess.run(command + list(extra), check=True, timeout=60)
            install('--panel-only')
            panel_env = parse_env(system / 'etc/chain-panel/panel.env')
            harness.launch('local-panel', source / 'bin/chain-panel', panel_env)
            harness.wait(lambda: harness.request('/') or True, 'local panel start')
            install()
            agent_env = parse_env(system / 'etc/chain-agent/agent.env')
            harness.launch('auto-agent', source / 'bin/chain-agent', agent_env)
            token = (system / 'var/lib/chain-panel/admin-token').read_text()
            harness.request('/api/login', 'POST', {'token': token})
            harness.wait(lambda: len(harness.state()['nodes']) == 1 and harness.state()['nodes'][0]['online'], 'automatically registered Agent online')
            state_file = system / 'opt/chain/.chain-install.json'
            previous = json.loads(state_file.read_text())['roles']['agent'].copy()
            identity = (system / 'etc/chain-agent/token').read_bytes()
            install()
            assert json.loads(state_file.read_text())['roles']['agent'] == previous
            assert (system / 'etc/chain-agent/token').read_bytes() == identity
            assert (system / 'usr/local/bin/chainctl').is_symlink()
            assert len(harness.state()['nodes']) == 1
            if sys.platform == 'darwin':
                marker = root / 'watcher.json'
                owner_code = '''import json, os, pathlib, subprocess, sys, time
child = subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(60)'])
watcher = subprocess.Popen([sys.argv[1], '--chain-watch', str(os.getpid()), str(child.pid)], stdout=subprocess.PIPE)
assert watcher.stdout.read(1) == b'R'
pathlib.Path(sys.argv[2]).write_text(json.dumps([child.pid, watcher.pid]))
time.sleep(60)
'''
                owner = harness.launch('parent-watch', sys.executable, {}, ['-c', owner_code, str(source / 'bin/chain-agent'), str(marker)])
                harness.wait(marker.exists, 'macOS process watcher registration')
                watched = json.loads(marker.read_text())
                os.kill(owner.pid, signal.SIGKILL)
                owner.wait(timeout=5)
                harness.wait(lambda: all(subprocess.run(['ps', '-p', str(pid)], stdout=subprocess.DEVNULL).returncode != 0
                                         for pid in watched), 'macOS cores and watchdog exit after owner SIGKILL')
            print('PASS installed service configuration, real local auto-enrollment, node heartbeat, and repeated installation preserving identity.', flush=True)
        except BaseException:
            for name, process, _ in harness.processes:
                print(f'{name} exit={process.poll()}:\n{(root / (name + ".log")).read_text(errors="replace")[-4000:]}', file=sys.stderr)
            raise
        finally:
            harness.close()
    assert not root.exists()


if __name__ == '__main__':
    signal.signal(signal.SIGTERM, lambda *_: (_ for _ in ()).throw(KeyboardInterrupt()))
    main()
