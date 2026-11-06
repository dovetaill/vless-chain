"""Exercise bootstrap package selection with fake executables; never install services."""
import hashlib
import io
from pathlib import Path
import os
import platform
import signal
import subprocess
import tarfile
import tempfile
import time

source = Path(__file__).resolve().parents[1] / 'install-server.sh'
with tempfile.TemporaryDirectory(prefix='chain-local-package-') as directory:
    root = Path(directory)
    mocks = root / 'mocks'
    mocks.mkdir()
    curl = mocks / 'curl'
    curl.write_text('#!/bin/sh\necho unexpected-network >&2\nexit 99\n')
    curl.chmod(0o700)
    fallback = root / 'disk tmp'
    fallback.mkdir()
    df = mocks / 'df'
    df.write_text('''#!/bin/sh
case "$2" in
  "$TEST_FALLBACK") available=${TEST_FALLBACK_KIB:-4194304};;
  "$TMPDIR") available=${TEST_TMP_KIB:-4194304};;
  *) exit 97;;
esac
[ "$available" != error ] || exit 1
printf 'Filesystem 1024-blocks Used Available Capacity Mounted on\\nfixture 8388608 1 %s 1%% %s\\n' "$available" "$2"
''')
    df.chmod(0o700)
    script = root / 'scripts' / 'install-server.sh'
    script.parent.mkdir()
    script_body = source.read_text().replace('PATH=/opt', 'PATH=' + str(mocks) + ':/opt', 1)
    candidates = 'in "${TMPDIR:-/tmp}" /var/tmp; do'
    assert script_body.count(candidates) == 1
    script.write_text(script_body.replace(candidates, 'in "${TMPDIR:-/tmp}" "' + str(fallback) + '"; do'))
    # A bundled stale executable must not override an explicitly supplied package.
    (root / 'bin').mkdir()
    (root / 'bin/chainctl').write_text('#!/bin/sh\nexit 98\n')
    (root / 'bin/chainctl').chmod(0o700)
    system = {'Linux': 'linux', 'Darwin': 'darwin'}[platform.system()]
    arch = {'arm64': 'aarch64'}.get(platform.machine(), platform.machine())
    packages = root / 'packages with spaces'
    packages.mkdir()
    bundle = packages / ('chain-panel-0.2.70-' + system + '-' + arch + '.tar.gz')
    body = b'''#!/bin/sh
set -eu
printf "%s\\n" "$@"
printf "%s\\n" "$TMPDIR" > "$TEST_NATIVE_TMPFILE"
mkdir "$TMPDIR/chain-native-install-fixture"
printf expanded > "$TMPDIR/chain-native-install-fixture/stage"
if [ "${TEST_NATIVE_WAIT:-}" = 1 ]; then
  exec python3 -c 'import os, signal; open(os.environ["TEST_NATIVE_PIDFILE"], "w").write(str(os.getpid())); signal.pause()'
fi
exit "${TEST_NATIVE_EXIT:-0}"
'''
    with tarfile.open(bundle, 'w:gz') as archive:
        entry = tarfile.TarInfo('bin/chainctl')
        entry.size = len(body)
        entry.mode = 0o755
        archive.addfile(entry, io.BytesIO(body))
    sums = packages / 'SHA256SUMS'
    sums.write_text(hashlib.sha256(bundle.read_bytes()).hexdigest() + '  ' + bundle.name + '\n')
    native_tmp = root / 'native-tmp'
    env = dict(os.environ, TMPDIR=str(root), TEST_FALLBACK=str(fallback), TEST_NATIVE_TMPFILE=str(native_tmp))
    def cleaned():
        assert not list(root.rglob('chain-bootstrap-*')), 'temporary bootstrap leaked'
        if native_tmp.exists():
            directory = Path(native_tmp.read_text().strip())
            assert directory.parent in (root, fallback), 'native extraction escaped selected directory'
            assert not directory.exists(), 'native extraction directory leaked'
    def call(args):
        native_tmp.unlink(missing_ok=True)
        result = subprocess.run(['sh', str(script), *args], cwd=root, env=env, capture_output=True, text=True, timeout=15)
        assert 'unexpected-network' not in result.stderr
        cleaned()
        return result
    args = ['update', '--package', str(bundle), '--non-interactive']
    result = call(args)
    assert result.returncode == 0, result.stderr
    assert result.stdout.splitlines() == args
    assert Path(native_tmp.read_text().strip()).parent == root, 'sufficient TMPDIR was ignored'
    result = call(['update', '--package', str(bundle.relative_to(root)), '--non-interactive'])
    assert result.returncode == 0, result.stderr
    renamed = packages / 'native.tar.gz'
    renamed.write_bytes(bundle.read_bytes())
    sums.write_text(hashlib.sha256(renamed.read_bytes()).hexdigest() + '  native.tar.gz\n')
    assert call(['update', '--package', str(renamed)]).returncode == 0
    renamed.unlink()
    sums.write_text(hashlib.sha256(bundle.read_bytes()).hexdigest() + '  ' + bundle.name + '\n')
    for arguments in [['update', '--package'], ['update', '--package', ''], args + ['--package', str(bundle)], ['update', '--package', str(root / 'missing')]]:
        assert call(arguments).returncode != 0
    sums.write_text('0' * 64 + '  ' + bundle.name + '\n')
    assert '发行包校验失败' in call(args).stderr
    sums.unlink()
    assert 'SHA256SUMS' in call(args).stderr
    sums.write_text(hashlib.sha256(bundle.read_bytes()).hexdigest() + '  ' + bundle.name + '\n')
    bundled = subprocess.run(['sh', str(script), 'update'], env=env, capture_output=True)
    assert bundled.returncode == 98, 'default bundled executor changed'
    (root / 'bin/chainctl').unlink()
    download_mock = """#!/bin/sh
previous= output= url=
for arg in "$@"; do
  [ "$previous" != -o ] || output=$arg
  case "$arg" in https://*) url=$arg;; esac
  previous=$arg
done
case "$url" in
  */releases/latest) printf '%s' 'https://github.com/dovetaill/vless-chain/releases/tag/v0.2.70';;
  *) cp "$TEST_PACKAGE_DIR/${url##*/}" "$output";;
esac
"""
    curl.write_text(download_mock)
    env['TEST_PACKAGE_DIR'] = str(packages)
    remote_url = 'https://download.example/releases/v0.2.70/' + bundle.name
    (root / 'bin/chainctl').write_text('#!/bin/sh\nexit 98\n')
    (root / 'bin/chainctl').chmod(0o700)
    remote = call(['update', '--package', remote_url, '--non-interactive'])
    assert remote.returncode == 0, remote.stderr
    forwarded = remote.stdout.splitlines()
    assert forwarded[:2] == ['update', '--package']
    assert Path(forwarded[2]).name == bundle.name
    assert forwarded[3:] == ['--non-interactive']
    piped = subprocess.run(['sh', '-s', '--', 'update', '--package', remote_url, '--non-interactive'],
                           input=script.read_text(), cwd=root, env=env, capture_output=True, text=True)
    assert piped.returncode == 0, piped.stderr
    assert piped.stdout.splitlines()[:2] == ['update', '--package']
    assert piped.stdout.splitlines()[3:] == ['--non-interactive']
    cleaned()
    assert '下载地址必须使用 HTTPS' in call(['update', '--package', remote_url.replace('https:', 'http:')]).stderr
    sums.write_text('0' * 64 + '  ' + bundle.name + '\n')
    assert '发行包校验失败' in call(['update', '--package', remote_url]).stderr
    sums.unlink()
    assert call(['update', '--package', remote_url]).returncode != 0
    sums.write_text(hashlib.sha256(bundle.read_bytes()).hexdigest() + '  ' + bundle.name + '\n')
    curl.write_text('#!/bin/sh\nexit 22\n')
    assert call(['update', '--package', remote_url]).returncode != 0
    curl.write_text(download_mock)
    (root / 'bin/chainctl').unlink()
    online = call(['update', '--non-interactive'])
    assert online.returncode == 0, online.stderr
    forwarded = online.stdout.splitlines()
    assert forwarded[:3] == ['update', '--non-interactive', '--package']
    assert Path(forwarded[3]).name == bundle.name

    for available in ['83968', '3145727', 'error', 'invalid']:
        env['TEST_TMP_KIB'] = available
        result = call([])
        assert result.returncode == 0, result.stderr
        assert result.stdout.splitlines()[0] == 'setup'
        assert Path(native_tmp.read_text().strip()).parent == fallback
        assert str(fallback) in result.stderr
    env['TEST_TMP_KIB'] = '3145728'
    assert call([]).returncode == 0
    assert Path(native_tmp.read_text().strip()).parent == root

    env['TEST_TMP_KIB'] = '83968'
    for available in ['83968', 'error', 'invalid']:
        env['TEST_FALLBACK_KIB'] = available
        curl.write_text('#!/bin/sh\necho unexpected-network >&2\nexit 99\n')
        result = call([])
        assert result.returncode != 0 and '3 GiB' in result.stderr
        assert not native_tmp.exists(), 'installer ran without enough space'
    del env['TEST_FALLBACK_KIB']
    curl.write_text(download_mock)
    env['TMPDIR'] = str(root / 'missing tmp')
    assert call([]).returncode == 0
    assert Path(native_tmp.read_text().strip()).parent == fallback
    env['TMPDIR'] = str(root)

    env['TEST_NATIVE_EXIT'] = '42'
    assert call([]).returncode == 42
    del env['TEST_NATIVE_EXIT']
    native = root / 'exported-chainctl'
    assert call(['bootstrap-native', str(native)]).returncode == 0
    assert native.read_bytes() == body
    native.unlink()
    curl.write_text('#!/bin/sh\nexit 22\n')
    assert call([]).returncode == 22
    curl.write_text(download_mock)

    pidfile = root / 'native.pid'
    env.update(TEST_NATIVE_WAIT='1', TEST_NATIVE_PIDFILE=str(pidfile))
    process = subprocess.Popen(['sh', str(script)], cwd=root, env=env, stdout=subprocess.PIPE,
                               stderr=subprocess.PIPE, text=True, start_new_session=True)
    print(f'interruption fixture: pgid={process.pid}, cwd={root}, pidfile={pidfile}, ports=none', flush=True)
    try:
        deadline = time.monotonic() + 10
        while not pidfile.exists() or not pidfile.read_text().strip():
            assert process.poll() is None and time.monotonic() < deadline, 'native executor did not start'
            time.sleep(0.02)
        process.terminate()
        stdout, stderr = process.communicate(timeout=10)
        assert process.returncode == 143, (stdout, stderr)
        cleaned()
        try:
            os.kill(int(pidfile.read_text()), 0)
        except ProcessLookupError:
            pass
        else:
            raise AssertionError('native executor survived bootstrap termination')
        try:
            os.killpg(process.pid, 0)
        except ProcessLookupError:
            pass
        else:
            raise AssertionError('bootstrap process group leaked')
    finally:
        try:
            os.killpg(process.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        process.communicate(timeout=10)

print('PASS HTTPS/local packages, GitHub, bundled executor, arguments, checksums, space fallback, unavailable directories, native TMPDIR, failure/termination cleanup')
