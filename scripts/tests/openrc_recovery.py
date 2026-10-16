"""Real OpenRC retry/rollback regression; run only in a disposable Alpine container."""
import os
from pathlib import Path
import pwd
import subprocess
import sys
import time
import urllib.request

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import chain_platform as platform


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
    name = 'chain-panel'
    platform.create_account(name, '/var/lib/' + name)
    user = pwd.getpwnam(name)
    data = Path('/var/lib/' + name)
    data.mkdir(mode=0o700)
    os.chown(data, user.pw_uid, user.pw_gid)
    keep = data / 'keep'
    keep.write_text('business data')
    baseline = (data.stat().st_uid, data.stat().st_gid, data.stat().st_mode)
    release = Path('/opt/chain/releases/fixture/bin')
    release.mkdir(parents=True)
    binary = release / name
    binary.write_text('#!/usr/bin/python3\nfrom http.server import HTTPServer, BaseHTTPRequestHandler\n'
                      'class Handler(BaseHTTPRequestHandler):\n'
                      ' def do_GET(self):\n'
                      '  self.send_response(200); self.end_headers(); self.wfile.write(b"ready")\n'
                      'HTTPServer(("127.0.0.1", 18080), Handler).serve_forever()\n')
    binary.chmod(0o755)
    current = Path('/opt/chain/current-panel')
    current.symlink_to(release.parent)
    unit = Path('/etc/init.d/' + name)
    pidfile = Path('/run/supervise-chain-panel.pid')
    definition = platform.service_definition(name, [current / 'bin' / name], 'chain-panel-installer-v1', name)

    def write_unit(text):
        unit.write_text(text)
        unit.chmod(0o755)

    def ready():
        for _ in range(50):
            try:
                with urllib.request.urlopen('http://127.0.0.1:18080/', timeout=1) as response:
                    assert response.read() == b'ready'
                return
            except OSError:
                time.sleep(0.1)
        raise AssertionError('service never became ready')

    # v0.2.24 overwrote OpenRC's helper PATH and used an unwritable log directory.
    write_unit(definition.replace(':"$PATH"', '').replace(str(data / 'service.log'), '/var/log/chain-panel.log'))
    failed = subprocess.run(['rc-service', name, 'restart'])
    assert failed.returncode != 0
    orphan = int(pidfile.read_text())
    assert Path(f'/proc/{orphan}').exists()
    unit.unlink()
    current.unlink()
    write_unit(definition)
    current.symlink_to(release.parent)
    platform.service('restart', name)
    ready()
    assert not Path(f'/proc/{orphan}').exists()
    first = int(pidfile.read_text())
    platform.service('restart', name)
    ready()
    assert int(pidfile.read_text()) != first and not Path(f'/proc/{first}').exists()
    platform.service('stop', name)

    # A failed start hook must not leave a supervisor behind after installer rollback.
    write_unit(definition + 'start_post() { return 1; }\n')
    failed = subprocess.run(['rc-service', name, 'start'])
    assert failed.returncode != 0
    orphan = int(pidfile.read_text())
    platform.service('stop', name, check=False)
    assert not Path(f'/proc/{orphan}').exists() and not pidfile.exists()
    write_unit(definition)

    # A recycled PID must never terminate an unrelated process.
    foreign = subprocess.Popen(['sleep', '300'])
    try:
        pidfile.write_text(str(foreign.pid))
        pidfile.chmod(0o600)
        try:
            platform.service('restart', name)
        except ValueError as error:
            assert '拒绝清理' in str(error)
        else:
            raise AssertionError('foreign process accepted')
        assert foreign.poll() is None
    finally:
        foreign.terminate()
        foreign.wait(timeout=5)
    platform.service('restart', name)
    ready()
    platform.service('stop', name)
    assert not pidfile.exists()
    assert keep.read_text() == 'business data'
    assert (data.stat().st_uid, data.stat().st_gid, data.stat().st_mode) == baseline
    print('OpenRC: failed-install retry, normal restart, rollback cleanup, PID protection and data retention passed')


if __name__ == '__main__':
    main()
