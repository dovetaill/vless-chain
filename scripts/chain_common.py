"""Shared release/installation primitives; no system mutation at import time."""
import contextlib
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import platform
import re
import shutil
import signal
import subprocess
import tarfile
import tempfile
import time
import urllib.error
import urllib.request

FORMAT = 1
VERSION_RE = r'[0-9]+\.[0-9]+\.[0-9]+'
RELEASE_REPO = 'dovetaill/vless-chain'


def download_release(directory):
    """Fetch the latest published native bundle from this project's release channel."""
    request = urllib.request.Request(f'https://api.github.com/repos/{RELEASE_REPO}/releases/latest',
                                     headers={'Accept': 'application/vnd.github+json', 'User-Agent': 'chainctl'})
    try:
        with urllib.request.urlopen(request, timeout=30) as response:
            payload = response.read(1024 * 1024 + 1)
        require(len(payload) <= 1024 * 1024, '发行信息过大。')
        release = json.loads(payload)
    except urllib.error.HTTPError as error:
        if error.code == 404:
            raise ValueError('仓库尚无已发布的安装包。请由维护者先发布 Release；已有本地发行包可在安装向导中选择。') from None
        raise ValueError(f'获取正式版本失败（HTTP {error.code}），请稍后重试或使用本地发行包。') from None
    tag = release.get('tag_name', '')
    require(re.fullmatch('v' + VERSION_RE, tag) and not release.get('draft') and not release.get('prerelease'),
            '正式版本信息无效。')
    name = f'chain-panel-{tag[1:]}-linux-{architecture()}.tar.gz'
    assets = {item['name']: item for item in release.get('assets', [])}
    require(name in assets and 'SHA256SUMS' in assets, f'正式版本 {tag} 尚未提供本机架构安装包及校验文件。')
    directory = Path(directory)
    print(f'正在下载正式版本 {tag}（{architecture()}）…', flush=True)
    for filename, maximum in [('SHA256SUMS', 65536), (name, 512 * 1024**2)]:
        url = assets[filename].get('browser_download_url', '')
        require(url == f'https://github.com/{RELEASE_REPO}/releases/download/{tag}/{filename}', '发行附件来源不匹配。')
        with urllib.request.urlopen(url, timeout=60) as response, (directory / filename).open('xb') as stream:
            require(response.geturl().startswith('https://'), '下载必须使用 HTTPS。')
            total = 0
            for chunk in iter(lambda: response.read(1024 * 1024), b''):
                total += len(chunk)
                require(total <= maximum, '发行附件大小超限。')
                stream.write(chunk)
    verify_archive_checksum(directory / name)
    return directory / name, tag[1:]


def require(condition, message):
    if not condition:
        raise ValueError(message)


def architecture():
    require(platform.system() == 'Linux', '当前发行包只支持 Linux。')
    value = {'x86_64': 'x86_64', 'aarch64': 'aarch64', 'arm64': 'aarch64'}.get(platform.machine())
    require(value, '仅支持 Linux x86_64 / aarch64 原生构建与安装。')
    return value


def sha256(path):
    digest = hashlib.sha256()
    with Path(path).open('rb') as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b''):
            digest.update(chunk)
    return digest.hexdigest()


def atomic_write(path, value, mode=0o600):
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    fd, name = tempfile.mkstemp(prefix='.' + path.name + '-', dir=path.parent)
    try:
        with os.fdopen(fd, 'wb') as stream:
            stream.write(value.encode() if isinstance(value, str) else value)
            stream.flush()
            os.fsync(stream.fileno())
        os.chmod(name, mode)
        os.replace(name, path)
    finally:
        with contextlib.suppress(FileNotFoundError):
            os.unlink(name)


def json_write(path, value, mode=0o600):
    atomic_write(path, json.dumps(value, ensure_ascii=False, indent=2) + '\n', mode)


def run(command, **kwargs):
    kwargs.setdefault('check', True)
    return subprocess.run([str(v) for v in command], **kwargs)


def output(command, **kwargs):
    return run(command, capture_output=True, text=True, **kwargs).stdout.strip()


def owned_processes(root):
    marker = f'CHAIN_TASK_OWNER={root}'.encode()
    result = []
    for path in Path('/proc').iterdir():
        if path.name.isdigit():
            with contextlib.suppress(OSError):
                if marker in (path / 'environ').read_bytes().split(b'\0'):
                    result.append(int(path.name))
    return result


class Workspace:
    """Owned build directory and process groups, cleaned on success or exception."""
    def __init__(self, parent, prefix='.chain-build-'):
        parent = Path(parent).resolve(strict=True)
        require(shutil.disk_usage(parent).free > 2 * 1024**3, '工作目录至少需要 2 GiB 可用空间。')
        self.root = Path(tempfile.mkdtemp(prefix=prefix, dir=parent))
        self.before = shutil.disk_usage(parent).free
        self.children = []
        self.manifest = {'pid': os.getpid(), 'pgid': os.getpgrp(), 'cwd': str(Path.cwd()),
                         'root': str(self.root), 'groups': [], 'ports': []}
        self.env = os.environ.copy()
        self.env.update(CHAIN_TASK_OWNER=str(self.root), CARGO_TARGET_DIR=str(self.root / 'target'),
                        CARGO_INCREMENTAL='0', CARGO_PROFILE_DEV_DEBUG='0', CARGO_PROFILE_TEST_DEBUG='0',
                        npm_config_cache=str(self.root / 'npm-cache'), npm_config_progress='false',
                        TMPDIR=str(self.root / 'tmp'), PYTHONDONTWRITEBYTECODE='1', CI='1')
        (self.root / 'tmp').mkdir()
        self.record()
        print(f'临时目录：{self.root}；拥有者 PID/PGID={os.getpid()}/{os.getpgrp()}；不启动构建监听服务。', flush=True)

    def record(self):
        json_write(self.root / 'ownership.json', self.manifest)

    def run(self, command, cwd=None, **kwargs):
        self.manifest['next_command'] = [str(v) for v in command]
        self.manifest['command_cwd'] = str(cwd or Path.cwd())
        self.record()
        process = subprocess.Popen([str(v) for v in command], cwd=cwd, env=self.env, start_new_session=True, **kwargs)
        self.children.append(process)
        self.manifest['groups'].append(process.pid)
        self.record()
        print(f'执行：{" ".join(str(v) for v in command)}；PID/PGID={process.pid}', flush=True)
        code = process.wait()
        if code:
            raise subprocess.CalledProcessError(code, command)

    def __enter__(self):
        return self

    def __exit__(self, *_):
        groups = set()
        for pid in owned_processes(self.root):
            with contextlib.suppress(ProcessLookupError):
                groups.add(os.getpgid(pid))
        groups.discard(os.getpgrp())
        for group in groups:
            with contextlib.suppress(ProcessLookupError):
                os.killpg(group, signal.SIGTERM)
        for process in self.children:
            with contextlib.suppress(subprocess.TimeoutExpired):
                process.wait(timeout=8)
        deadline = time.monotonic() + 5
        while owned_processes(self.root) and time.monotonic() < deadline:
            time.sleep(.1)
        for pid in owned_processes(self.root):
            with contextlib.suppress(ProcessLookupError):
                os.kill(pid, signal.SIGKILL)
        for process in self.children:
            with contextlib.suppress(subprocess.TimeoutExpired):
                process.wait(timeout=3)
        remaining = owned_processes(self.root)
        require(not remaining, f'清理未完成：PID={remaining}；保留目录 {self.root}')
        held = []
        for proc in Path('/proc').iterdir():
            if not proc.name.isdigit():
                continue
            with contextlib.suppress(OSError):
                links = [proc / 'cwd', proc / 'exe', *(proc / 'fd').iterdir()]
                for link in links:
                    with contextlib.suppress(OSError):
                        if str(link.readlink()).startswith(str(self.root) + '/'):
                            held.append(f'{proc.name}:{link.name}')
        require(not held, f'仍有进程持有临时文件：{held}；保留目录 {self.root}')
        size = sum(p.stat().st_size for p in self.root.rglob('*') if p.is_file())
        shutil.rmtree(self.root)
        after = shutil.disk_usage(self.root.parent).free
        print(f'清理完成：命令进程组 {self.manifest["groups"]} 已退出；临时目录 {size} → 0 字节；'
              f'磁盘可用空间 {self.before} → {after} 字节。', flush=True)


def safe_extract(archive, destination):
    """Reject traversal, duplicates, links, special files and archive bombs."""
    destination = Path(destination)
    destination.mkdir(mode=0o700, parents=True, exist_ok=False)
    with tarfile.open(archive, 'r:gz') as bundle:
        members = bundle.getmembers()
        require(len(members) <= 300, '发行包文件数量超限。')
        require(sum(m.size for m in members) <= 1024**3, '发行包展开体积超过 1 GiB。')
        seen = set()
        for member in members:
            path = PurePosixPath(member.name)
            require(member.isfile() and not path.is_absolute() and '..' not in path.parts
                    and str(path) == member.name and member.name not in seen,
                    '发行包含不安全路径、链接、重复或特殊文件：' + member.name)
            seen.add(member.name)
        for member in members:
            path = destination / member.name
            path.parent.mkdir(parents=True, exist_ok=True)
            with bundle.extractfile(member) as source, path.open('wb') as target:
                shutil.copyfileobj(source, target)
            path.chmod(0o600)
    return verify_tree(destination)


def verify_tree(directory):
    directory = Path(directory)
    manifest = json.loads((directory / 'release.json').read_text())
    require(manifest.get('format') == FORMAT and re.fullmatch(VERSION_RE, manifest.get('version', '')),
            '发行包格式或版本无效。')
    files = manifest.get('files', {})
    required = {'bin/chain-panel', 'bin/chain-agent', 'bin/xray', 'scripts/chain_common.py',
                'scripts/chain_install.py', 'scripts/chain_network.py', 'scripts/install-server.sh'}
    require(required.issubset(files), '发行包缺少程序或安装脚本。')
    actual = set()
    for path in directory.rglob('*'):
        require(not path.is_symlink(), '发行目录不得包含符号链接。')
        if path.is_file():
            actual.add(str(path.relative_to(directory)))
    require(actual == set(files) | {'release.json'}, '发行包文件清单与实际内容不一致。')
    for name, info in files.items():
        require(re.fullmatch(r'[0-9a-f]{64}', info.get('sha256', '')) is not None, '清单摘要无效。')
        require(sha256(directory / name) == info['sha256'], '发行文件校验失败：' + name)
        require(info.get('mode') in (0o644, 0o755), '发行文件权限无效。')
    return manifest


def verify_archive_checksum(archive):
    archive = Path(archive)
    checksum = archive.parent / 'SHA256SUMS'
    require(checksum.is_file(), f'缺少 {checksum}；请将校验文件与发行包放在同一目录。')
    pairs = [line.split() for line in checksum.read_text().splitlines() if line.strip()]
    matches = [p[0] for p in pairs if len(p) == 2 and p[1] == archive.name]
    require(len(matches) == 1 and matches[0] == sha256(archive), '安装包 SHA256 不匹配。')
    return matches[0]


def restore_modes(directory, manifest):
    directory = Path(directory)
    for path in [directory, *directory.rglob('*')]:
        if path.is_dir():
            path.chmod(0o755)
    for name, info in manifest['files'].items():
        (directory / name).chmod(info['mode'])
    (directory / 'release.json').chmod(0o644)
