"""Build-only pinned core downloads; installed servers use Rust chainctl."""
import gzip
import json
import os
from pathlib import Path
import re
import shutil
import stat
import urllib.request
import zipfile
from chain_common import VERSION_RE, architecture, operating_system, require, sha256
RECOMMENDED = json.loads((Path(__file__).resolve().parents[1] / 'panel/src/core-versions.json').read_text())
REPOS = {'xray': 'XTLS/Xray-core', 'mihomo': 'MetaCubeX/mihomo'}
LIMIT = 128 * 1024**2


def fetch(url, path, limit):
    request = urllib.request.Request(url, headers={'User-Agent': 'chainctl', 'Accept': 'application/vnd.github+json'})
    token = os.environ.get('GH_TOKEN')
    if token and url.startswith('https://api.github.com/'):
        request.add_unredirected_header('Authorization', 'Bearer ' + token)
    with urllib.request.urlopen(request, timeout=60) as response, Path(path).open('xb') as stream:
        require(response.geturl().startswith('https://'), '内核下载必须使用 HTTPS')
        total = 0
        while chunk := response.read(1024**2):
            total += len(chunk)
            require(total <= limit, '内核下载大小超限')
            stream.write(chunk)


def asset(release, core, version, allow_prerelease=False):
    require(core in REPOS and re.fullmatch(VERSION_RE, version), '内核版本无效')
    require(release.get('tag_name') == 'v' + version and release.get('draft') is False, '官方内核版本信息不匹配')
    require(not release.get('prerelease') or allow_prerelease, '这是预发布版本，请明确选择后重试')
    os_name, arch = operating_system(), architecture()
    if core == 'xray':
        suffix = {('linux', 'x86_64'): 'linux-64', ('linux', 'aarch64'): 'linux-arm64-v8a',
                  ('darwin', 'x86_64'): 'macos-64', ('darwin', 'aarch64'): 'macos-arm64-v8a'}[os_name, arch]
        name = f'Xray-{suffix}.zip'
    else:
        name = f'mihomo-{os_name}-{"amd64-compatible" if arch == "x86_64" else "arm64"}-v{version}.gz'
    matches = [a for a in release.get('assets', []) if a.get('name') == name]
    url = f'https://github.com/{REPOS[core]}/releases/download/v{version}/{name}'
    require(len(matches) == 1 and matches[0].get('browser_download_url') == url, '官方内核架构附件缺失或来源不匹配')
    digest = matches[0].get('digest', '')
    require(isinstance(digest, str) and re.fullmatch(r'sha256:[0-9a-f]{64}', digest), '官方附件缺少有效 SHA256')
    return name, url, digest[7:]


def download(core, version, directory, destination, allow_prerelease=False):
    directory, destination = Path(directory), Path(destination)
    metadata = directory / (core + '-release.json')
    fetch(f'https://api.github.com/repos/{REPOS[core]}/releases/tags/v{version}', metadata, 2 * 1024**2)
    release = json.loads(metadata.read_text())
    name, url, digest = asset(release, core, version, allow_prerelease)
    archive = directory / name
    fetch(url, archive, LIMIT)
    require(sha256(archive) == digest, '内核 SHA256 校验失败，未替换内核')
    destination.parent.mkdir(parents=True, exist_ok=True)
    share = destination.parent.parent / 'share'
    share.mkdir(exist_ok=True)
    if core == 'xray':
        with zipfile.ZipFile(archive) as bundle:
            require(len(bundle.infolist()) <= 100, 'Xray 附件文件数量超限')
            for name, target in [('xray', destination), ('LICENSE', share / 'Xray-LICENSE')]:
                entries = [e for e in bundle.infolist() if e.filename == name]
                require(len(entries) == 1 and not entries[0].is_dir() and entries[0].file_size <= LIMIT
                        and not stat.S_ISLNK(entries[0].external_attr >> 16), 'Xray 附件条目无效')
                with bundle.open(entries[0]) as source:
                    content = source.read(LIMIT + 1)
                require(len(content) == entries[0].file_size and len(content) <= LIMIT, 'Xray 解压大小无效')
                target.write_bytes(content)
    else:
        with gzip.open(archive, 'rb') as source:
            content = source.read(LIMIT + 1)
        require(0 < len(content) <= LIMIT, 'Mihomo 解压大小无效')
        destination.write_bytes(content)
        license_path = directory / 'Mihomo-LICENSE'
        fetch(f'https://raw.githubusercontent.com/{REPOS[core]}/v{version}/LICENSE', license_path, 65536)
        shutil.copyfile(license_path, share / 'Mihomo-LICENSE')
    destination.chmod(0o755)
    return {'version': version, 'source': url, 'archive_sha256': digest, 'prerelease': bool(release.get('prerelease'))}
