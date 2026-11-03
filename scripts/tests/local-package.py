"""Exercise bootstrap package selection with fake executables; never install services."""
import hashlib
import io
from pathlib import Path
import os
import platform
import subprocess
import tarfile
import tempfile

source = Path(__file__).resolve().parents[1] / 'install-server.sh'
with tempfile.TemporaryDirectory(prefix='chain-local-package-') as directory:
    root = Path(directory)
    mocks = root / 'mocks'
    mocks.mkdir()
    curl = mocks / 'curl'
    curl.write_text('#!/bin/sh\necho unexpected-network >&2\nexit 99\n')
    curl.chmod(0o700)
    script = root / 'scripts' / 'install-server.sh'
    script.parent.mkdir()
    script.write_text(source.read_text().replace('PATH=/opt', 'PATH=' + str(mocks) + ':/opt', 1))
    # A bundled stale executable must not override an explicitly supplied package.
    (root / 'bin').mkdir()
    (root / 'bin/chainctl').write_text('#!/bin/sh\nexit 98\n')
    (root / 'bin/chainctl').chmod(0o700)
    system = {'Linux': 'linux', 'Darwin': 'darwin'}[platform.system()]
    arch = {'arm64': 'aarch64'}.get(platform.machine(), platform.machine())
    packages = root / 'packages with spaces'
    packages.mkdir()
    bundle = packages / ('chain-panel-0.2.70-' + system + '-' + arch + '.tar.gz')
    body = b'#!/bin/sh\nprintf "%s\\n" "$@"\n'
    with tarfile.open(bundle, 'w:gz') as archive:
        entry = tarfile.TarInfo('bin/chainctl')
        entry.size = len(body)
        entry.mode = 0o755
        archive.addfile(entry, io.BytesIO(body))
    sums = packages / 'SHA256SUMS'
    sums.write_text(hashlib.sha256(bundle.read_bytes()).hexdigest() + '  ' + bundle.name + '\n')
    env = dict(os.environ, TMPDIR=str(root))
    def call(args):
        result = subprocess.run(['sh', str(script), *args], cwd=root, env=env, capture_output=True, text=True)
        assert 'unexpected-network' not in result.stderr
        assert not list(root.glob('chain-bootstrap-*')), 'temporary bootstrap leaked'
        return result
    args = ['update', '--package', str(bundle), '--non-interactive']
    result = call(args)
    assert result.returncode == 0, result.stderr
    assert result.stdout.splitlines() == args
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
    assert not list(root.glob('chain-bootstrap-*'))
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

print('PASS HTTPS package, default GitHub, download failure, local package, bundled executor bypass, spaces, relative path, missing/duplicate arguments, checksum rejection and cleanup')
