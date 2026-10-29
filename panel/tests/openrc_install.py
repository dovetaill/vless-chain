"""Native installation, OpenRC recovery and maintenance in a disposable container."""
import hashlib
import http.cookiejar
import json
import os
from pathlib import Path
import pwd
import socket
import ssl
import subprocess
import sys
import tarfile
import tempfile
import time
import urllib.request
import urllib.error
import uuid


def time_checks(ctl, work):
    fake = work / 'time-tools'; fake.mkdir()
    program = fake / 'tool'
    program.write_text("""#!/usr/bin/python3
import json,os,pathlib,sys
name=pathlib.Path(sys.argv[0]).name; args=sys.argv[1:]
with open(os.environ['TIME_CALLS'],'a') as f:f.write(json.dumps([name,*args])+'\\n')
c=json.loads(os.environ['TIME_CASE'])
if name=='timedatectl':print('Timezone=America/New_York\\nNTP=yes\\nNTPSynchronized=yes')
elif name=='systemctl' and args[0]=='list-unit-files':
 print(c['units'],end='')
 if c.get('inventory_error'):print('inventory denied',file=sys.stderr)
 sys.exit(c.get('inventory_code',0))
elif name=='systemctl' and args[0]=='is-active':sys.exit(0 if args[-1] in c.get('active',[]) else 3)
elif name=='chronyc' and args[0]=='waitsync' and c.get('no_source'):sys.exit(1)
""")
    program.chmod(0o755)
    for name in ['timedatectl', 'systemctl', 'chronyc', 'apt-get']:
        (fake / name).symlink_to(program)
    (fake / 'env').symlink_to('/usr/bin/env')
    cases = [
        {'units':'chrony.service enabled\nchronyd.service alias\n', 'active':['chrony.service']},
        {'units':'', 'inventory_code':1},
        {'units':'', 'inventory_code':1, 'inventory_error':True},
        {'units':'chrony.service enabled\nsystemd-timesyncd.service enabled\n', 'active':['chrony.service','systemd-timesyncd.service']},
        {'units':'chrony.service enabled\n', 'no_source':True},
        {'units':'chrony.service disabled\nsystemd-timesyncd.service enabled\n', 'active':['systemd-timesyncd.service']},
    ]
    for index, case in enumerate(cases):
        calls = work / f'time-calls-{index}'
        result = subprocess.run([str(ctl), 'sync-time'], env={**os.environ, 'PATH':str(fake),
            'TIME_CASE':json.dumps(case), 'TIME_CALLS':str(calls)}, capture_output=True, text=True, timeout=20)
        recorded = [json.loads(line) for line in calls.read_text().splitlines()]
        assert (result.returncode == 0) == (index in (0,1,5)), (index,result.stdout,result.stderr)
        assert not any('set-timezone' in row for row in recorded)
        if index in (2,3):
            assert not any(row[0]=='apt-get' or row[1:2]==['enable'] for row in recorded)
        if index==4:assert ['chronyc','makestep'] not in recorded
        if index==1:assert ['apt-get','install','-y','chrony'] in recorded
        if index==5:assert not any(row[0]=='chronyc' for row in recorded)
        if result.returncode==0:assert 'America/New_York' in result.stdout
    print('PASS native time synchronization decisions with isolated command fixtures', flush=True)


def main():
    assert os.environ.get('CHAIN_TEST_OPENRC') == '1' and Path('/.dockerenv').is_file()
    assert os.geteuid() == 0 and not Path('/opt/chain').exists()
    source = Path(sys.argv[1]).resolve()
    runtime = Path('/run/openrc')
    runtime.mkdir(exist_ok=True)
    (runtime / 'softlevel').write_text('default\n')
    net = Path('/etc/init.d/net')
    net.write_text('#!/sbin/openrc-run\nstart() { return 0; }\nstop() { return 0; }\n')
    net.chmod(0o755)
    subprocess.run(['rc-service', 'net', 'start'], check=True)
    previous = None
    if sys.argv[2] == 'recovery':
        subprocess.run(['addgroup', '-S', 'chain-panel'], check=True)
        subprocess.run(['adduser', '-S', '-D', '-H', '-h', '/var/lib/chain-panel', '-s', '/sbin/nologin', '-G', 'chain-panel', 'chain-panel'], check=True)
        user = pwd.getpwnam('chain-panel')
        data = Path('/var/lib/chain-panel'); data.mkdir(mode=0o700)
        os.chown(data, user.pw_uid, user.pw_gid)
        log = data / 'service.log'; log.write_text('previous failed install\n'); log.chmod(0o600)
        os.chown(log, user.pw_uid, user.pw_gid)
        previous = (log.read_bytes(), log.stat().st_uid, log.stat().st_gid, log.stat().st_mode)
        base = Path('/opt/chain'); (base / 'releases').mkdir(parents=True)
        state = base / '.chain-install.json'
        state.write_text(json.dumps({'owner': 'chain-panel-installer-v1', 'roles': {}})); state.chmod(0o600)
    with tempfile.TemporaryDirectory(prefix='native-openrc-') as temporary:
        work = Path(temporary)
        manifest = json.loads((source / 'release.json').read_text())
        archive = work / 'native.tar.gz'
        with tarfile.open(archive, 'w:gz') as bundle:
            for name in [*manifest['files'], 'release.json']:
                bundle.add(source / name, arcname=name, recursive=False)
        (work / 'SHA256SUMS').write_text(hashlib.sha256(archive.read_bytes()).hexdigest() + '  native.tar.gz\n')
        ctl = source / 'bin/chainctl'
        install = [str(ctl), 'install', '--package', str(archive), '--non-interactive',
                   '--listen', '127.0.0.1:37243', '--local-host', '127.0.0.1']
        def run(*args):
            result = subprocess.run([str(ctl), *args], timeout=120, capture_output=True, text=True)
            assert result.returncode == 0, result.stdout + result.stderr
            return result
        subprocess.run(install, check=True, timeout=120)
        wrapper = Path('/usr/local/bin/chainctl')
        assert wrapper.is_symlink() and wrapper.resolve().name == 'chainctl'
        for role in ['panel', 'agent']:
            subprocess.run(['rc-service', 'chain-' + role, 'status'], check=True)
            unit = Path('/etc/init.d/chain-' + role).read_text()
            assert 'python' not in unit and ':"$PATH"' in unit
            assert Path('/var/lib/chain-' + role, '.chain-owner').read_text() == 'chain-' + role
        if previous:
            log = Path('/var/lib/chain-panel/service.log')
            assert log.read_bytes().startswith(previous[0])
            assert (log.stat().st_uid, log.stat().st_gid, log.stat().st_mode) == previous[1:]
        token = Path('/etc/chain-agent/token').read_bytes()
        admin = Path('/var/lib/chain-panel/admin-token').read_bytes()
        node = Path('/var/lib/chain-panel/local-system-node').read_bytes()
        jar = http.cookiejar.CookieJar()
        client = urllib.request.build_opener(urllib.request.ProxyHandler({}), urllib.request.HTTPCookieProcessor(jar))
        api_base = 'http://127.0.0.1:37243'
        def api(path, value=None):
            request = urllib.request.Request(api_base + '/api/' + path,
                data=None if value is None else json.dumps(value).encode(),
                headers={'Content-Type': 'application/json', 'X-Chain-Request': '1'})
            with client.open(request, timeout=10) as response:
                return json.load(response)
        def ready():
            deadline = time.monotonic() + 30
            while True:
                try:
                    api('login', {'token': admin.decode().strip()})
                    break
                except urllib.error.URLError as error:
                    assert time.monotonic() < deadline, error
                    time.sleep(.1)
            while not api('state')['nodes'][0]['online']:
                assert time.monotonic() < deadline
                time.sleep(.2)
        ready()
        if sys.argv[2] == 'recovery':
            unit = Path('/etc/init.d/chain-panel')
            definition = unit.read_text()
            pidfile = Path('/run/supervise-chain-panel.pid')
            run('stop', '--component', 'panel')
            unit.write_text(definition + '\nstart_post() { return 1; }\n')
            failed = subprocess.run(['rc-service', 'chain-panel', 'start'], capture_output=True)
            assert failed.returncode != 0
            orphan = int(pidfile.read_text())
            run('stop', '--component', 'panel')
            assert not Path(f'/proc/{orphan}').exists() and not pidfile.exists()
            unit.write_text(definition)
            foreign = subprocess.Popen(['sleep', '300'])
            try:
                pidfile.write_text(str(foreign.pid)); pidfile.chmod(0o600)
                result = subprocess.run([str(ctl), 'restart', '--component', 'panel'], capture_output=True, text=True, timeout=30)
                assert result.returncode != 0 and foreign.poll() is None, result.stderr
            finally:
                foreign.terminate(); foreign.wait(timeout=5)
                pidfile.unlink(missing_ok=True)
            run('start', '--component', 'panel')
            ready()
        certdir = Path('/etc/native-test-cert'); certdir.mkdir()
        cert, key = certdir / 'cert.pem', certdir / 'key.pem'
        subprocess.run(['openssl','req','-x509','-newkey','ec','-pkeyopt','ec_paramgen_curve:prime256v1',
            '-nodes','-days','1','-subj','/CN=panel.test','-addext','subjectAltName=DNS:panel.test',
            '-out',str(cert),'-keyout',str(key)],check=True,capture_output=True)
        with Path('/etc/hosts').open('a') as stream: stream.write('\n127.0.0.1 panel.test\n')
        state_path = Path('/opt/chain/.chain-install.json')
        saved = json.loads(state_path.read_text())
        saved['roles']['panel']['manual_https'] = {'port':37443,'cert':str(cert),'key':str(key)}
        state_path.write_text(json.dumps(saved))
        install += ['--domain','panel.test']
        subprocess.run(install, check=True, timeout=120)
        context = ssl.create_default_context(cafile=str(cert))
        client = urllib.request.build_opener(urllib.request.ProxyHandler({}),
            urllib.request.HTTPSHandler(context=context),urllib.request.HTTPCookieProcessor(jar))
        api_base = 'https://panel.test:37443'
        ready()
        with client.open(api_base, timeout=10) as response:
            assert b'<!doctype html>' in response.read().lower()
        assert Path('/etc/chain-panel-https/nginx.conf').is_file()
        assert len(api('state')['nodes']) == 1
        assert Path('/etc/chain-agent/token').read_bytes() == token
        assert Path('/var/lib/chain-panel/admin-token').read_bytes() == admin
        assert Path('/var/lib/chain-panel/local-system-node').read_bytes() == node
        path = '/run/chain-maintenance/control.sock'
        panel_user = pwd.getpwnam('chain-panel')
        def request(value, uid=panel_user.pw_uid):
            code = "import socket,json,sys; s=socket.socket(socket.AF_UNIX); s.settimeout(10); s.connect(sys.argv[1]); s.sendall(sys.argv[2].encode()+b'\\n'); print(s.makefile('rb').readline().decode())"
            result = subprocess.run([sys.executable, '-B', '-c', code, path, json.dumps(value)],
                user=uid, group=panel_user.pw_gid, extra_groups=[], capture_output=True, text=True, timeout=15)
            assert result.returncode == 0, result.stderr
            return json.loads(result.stdout)
        deadline = time.monotonic() + 15
        while not Path(path).exists():
            assert time.monotonic() < deadline
            time.sleep(.1)
        info = Path(path).stat()
        assert (info.st_uid, info.st_gid, info.st_mode & 0o777) == (0, panel_user.pw_gid, 0o660)
        pidfile = Path('/run/supervise-chain-maintenance.pid')
        pid = pidfile.read_text()
        run('internal-adopt')
        assert pidfile.read_text() == pid and Path(path).stat().st_ino == info.st_ino
        task = str(uuid.uuid4())
        assert 'error' in request({'id': task, 'action': 'restart-all'}, pwd.getpwnam('nobody').pw_uid)
        assert 'error' in request({'id': task, 'action': 'restart-all', 'command': 'id'})
        assert request({'id': task, 'action': 'status'})['state'] == 'not_found'
        assert request({'id': task, 'action': 'restart-all'})['state'] == 'queued'
        deadline = time.monotonic() + 60
        while True:
            job = request({'id': task, 'action': 'status'})
            if job['state'] not in ('queued', 'running'):
                break
            assert time.monotonic() < deadline
            time.sleep(.2)
        assert job['state'] == 'succeeded', job
        assert request({'id': task, 'action': 'restart-all'}) == job
        assert 'error' in request({'id': task, 'action': 'restart-panel'})
        subprocess.run(['rc-service', 'chain-maintenance', 'restart'], check=True)
        assert request({'id': task, 'action': 'status'})['state'] == 'succeeded'
        ready()
        run('update', '--package', str(archive), '--non-interactive')
        ready()
        assert Path('/etc/chain-agent/token').read_bytes() == token
        run('uninstall', '--non-interactive')
        assert not wrapper.exists()
        assert Path('/var/lib/chain-panel/admin-token').read_bytes() == admin
        subprocess.run(install, check=True, timeout=120)
        ready()
        assert Path('/etc/chain-agent/token').read_bytes() == token
        run('uninstall', '--purge', '--non-interactive')
        assert not Path('/var/lib/chain-agent').exists()
        assert not Path('/var/lib/chain-panel').exists()
        time_checks(ctl, work)
        print('PASS native OpenRC install/recovery, identity, maintenance receipts, update, uninstall and reinstall', flush=True)


if __name__ == '__main__':
    try:
        main()
    except BaseException:
        for path in [Path('/var/lib/chain-panel/service.log'), Path('/var/lib/chain-agent/service.log')]:
            if path.exists():
                print(f'{path}:\n{path.read_text(errors="replace")[-4000:]}', flush=True)
        subprocess.run(['ps', '-ef'])
        raise
