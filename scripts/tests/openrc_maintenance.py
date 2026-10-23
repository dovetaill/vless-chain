"""Run only in a disposable Alpine container; exercises real OpenRC and Unix sockets."""
import json
import os
from pathlib import Path
import pwd
import socket
import subprocess
import sys
import time
import uuid

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from chain_common import json_write
from chain_install import Manager, OWNER
from chain_platform import create_account, service_definition


def main():
    assert os.environ.get('CHAIN_TEST_OPENRC') == '1' and Path('/.dockerenv').exists()
    assert os.geteuid() == 0 and not Path('/opt/chain').exists()
    runtime = Path('/run/openrc')
    runtime.mkdir(exist_ok=True)
    (runtime / 'softlevel').write_text('default\n')
    net = Path('/etc/init.d/net')
    net.write_text('#!/sbin/openrc-run\nstart() { return 0; }\nstop() { return 0; }\n')
    net.chmod(0o755)
    subprocess.run(['rc-service', 'net', 'start'], check=True)
    create_account('chain-panel', '/var/lib/chain-panel')
    create_account('chain-agent', '/var/lib/chain-agent')
    panel = pwd.getpwnam('chain-panel')
    manager = Manager(Path('/'))
    scripts = manager.base / 'current-panel/scripts'
    scripts.parent.mkdir(parents=True)
    scripts.symlink_to(Path(__file__).resolve().parents[1])
    json_write(manager.state_file, {'owner': OWNER, 'roles': {
        'panel': {'version': '1.2.3'}, 'agent': {'version': '1.2.3'}}})
    for role in ('panel', 'agent'):
        unit = Path('/etc/init.d/chain-' + role)
        unit.write_text(service_definition('chain-' + role, ['/bin/sleep', '3600'], OWNER))
        unit.chmod(0o755)
        subprocess.run(['rc-service', 'chain-' + role, 'start'], check=True)
    manager.maintenance_units()
    path = Path('/run/chain-maintenance/control.sock')

    def wait_for(predicate):
        for _ in range(150):
            if predicate():
                return
            time.sleep(.1)
        raise AssertionError('Timed out')

    def listening():
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as probe:
            probe.settimeout(.2)
            try:
                probe.connect(str(path))
                return True
            except OSError:
                return False

    wait_for(listening)
    info = path.stat()
    assert (info.st_uid, info.st_gid, info.st_mode & 0o777) == (0, panel.pw_gid, 0o660)
    # Repair invoked by an upgrade must not stop the executor executing that upgrade.
    pidfile = Path('/run/supervise-chain-maintenance.pid')
    pid = pidfile.read_text()
    manager.maintenance_units()
    assert pidfile.read_text() == pid and path.stat().st_ino == info.st_ino

    def request(value, uid=panel.pw_uid):
        code = ('import socket,json,sys; s=socket.socket(socket.AF_UNIX); s.settimeout(5); '
                's.connect(sys.argv[1]); s.sendall((sys.argv[2]+"\\n").encode()); '
                'print(s.recv(65536).decode()); s.close()')
        result = subprocess.run([sys.executable, '-B', '-c', code, str(path), json.dumps(value)],
                                user=uid, group=panel.pw_gid, extra_groups=[],
                                capture_output=True, text=True, timeout=10)
        assert result.returncode == 0, result.stderr
        return json.loads(result.stdout)

    job = {'id': str(uuid.uuid4()), 'action': 'restart-all'}
    assert 'error' in request(job, pwd.getpwnam('nobody').pw_uid)
    assert request(job)['state'] == 'queued'
    receipt = {}
    def complete():
        receipt.update(request({'id': job['id'], 'action': 'status'}))
        return receipt.get('state') not in ('queued', 'running')
    wait_for(complete)
    assert receipt['state'] == 'succeeded', receipt
    assert request(job)['finished'] == receipt['finished']
    assert 'error' in request({'id': str(uuid.uuid4()), 'action': 'shell'})
    subprocess.run(['rc-service', 'chain-maintenance', 'restart'], check=True)
    wait_for(listening)
    assert request(job)['state'] == 'succeeded'
    assert not list(Path('/var/lib/chain-maintenance').glob('work-*'))
    for name in ('chain-maintenance', 'chain-panel', 'chain-agent'):
        subprocess.run(['rc-service', name, 'stop'], check=True)
        assert subprocess.run(['rc-service', name, 'status']).returncode != 0
    print('PASS: OpenRC install/repair, panel-user socket, restart-all, receipts, restart recovery')


if __name__ == '__main__':
    main()
