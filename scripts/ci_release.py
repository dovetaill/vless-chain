"""Validate supported native builds and publish a complete, immutable Release."""
import argparse
import json
import os
from pathlib import Path
import re
import shutil
import signal
import subprocess
import sys

from chain_common import VERSION_RE, Workspace, atomic_write, output, require, restore_modes, run, safe_extract, sha256, verify_archive_checksum
from chain_release import version

PLATFORMS = (('linux', 'x86_64'), ('linux', 'aarch64'), ('darwin', 'aarch64'))


def validate_tag(tag):
    require(re.fullmatch('v' + VERSION_RE, tag), '只能发布 vX.Y.Z 正式版本标签。')
    require(tag == 'v' + version(), '标签与 Cargo/npm 版本不一致。')


def assemble(source, destination, tag, commit):
    validate_tag(tag)
    require(not destination.exists(), '聚合输出目录已存在，拒绝覆盖。')
    with Workspace(source.parent) as work:
        final = work.root / 'final'
        final.mkdir()
        sums = []
        for system, arch in PLATFORMS:
            directory = source / f'release-{system}-{arch}'
            name = f'chain-panel-{tag[1:]}-{system}-{arch}.tar.gz'
            archive = directory / name
            verify_archive_checksum(archive)
            manifest = safe_extract(archive, work.root / f'{system}-{arch}')
            require(manifest == json.loads((directory / 'release.json').read_text()), '包内外清单不一致。')
            require(manifest['version'] == tag[1:] and manifest['arch'] == arch
                    and manifest['os'] == system
                    and manifest['target'] == (f'{arch}-unknown-linux-musl' if system == 'linux' else f'{arch}-apple-darwin')
                    and manifest['commit'] == commit and manifest['dirty'] is False,
                    '架构、版本或提交来源不一致，拒绝发布。')
            require(manifest.get('libc') == 'musl' if system == 'linux' else manifest.get('macos_min') == '14.0',
                    '构建环境改变了最低系统要求。')
            shutil.copy2(archive, final / name)
            shutil.copy2(directory / 'release.json', final / f'release-{system}-{arch}.json')
            sums.append(f'{sha256(archive)}  {name}\n')
        atomic_write(final / 'SHA256SUMS', ''.join(sums), 0o644)
        shutil.copytree(final, destination)


def publish(directory, tag, repo, commit):
    validate_tag(tag)
    require(re.fullmatch(r'[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+', repo), '仓库名称无效。')
    expected = {'SHA256SUMS'} | {f'chain-panel-{tag[1:]}-{s}-{a}.tar.gz' for s, a in PLATFORMS} | {
        f'release-{s}-{a}.json' for s, a in PLATFORMS}
    require({p.name for p in directory.iterdir()} == expected, '发布附件不完整。')
    for system, arch in PLATFORMS:
        verify_archive_checksum(directory / f'chain-panel-{tag[1:]}-{system}-{arch}.tar.gz')
    releases = json.loads(output(['gh', 'api', f'repos/{repo}/releases?per_page=100']))
    existing = next((item for item in releases if item['tag_name'] == tag), None)
    if existing:
        require(existing['draft'], '此版本已经正式发布，不覆盖已有附件。')
        require(existing['target_commitish'] == commit, '已有草稿来自其他提交，拒绝覆盖。')
    else:
        notes = ('一条命令安装控制台与本机节点：\n\n'
                 '```bash\ncurl -fsSL https://raw.githubusercontent.com/' + repo +
                 '/main/scripts/install-server.sh | sudo sh\n```\n\n'
                 '首次只需输入可选域名。自动读取网卡公网 IPv4、选择空闲随机端口、生成登录令牌。'
                 '填写域名后检查 A/AAAA 解析，通过系统包管理器安装 Nginx/Certbot，配置 HTTPS 与自动续期。\n\n'
                 '支持 Linux（systemd/OpenRC）的 x86_64 / ARM64 与 macOS 14+ 的 Apple Silicon、Python ≥ 3.10；Linux 使用静态 musl 包，macOS 依赖 Homebrew。'
                 '下载对应架构的 tar.gz 与 SHA256SUMS 可离线安装。')
        release_notes = Path(__file__).resolve().parents[1] / 'panel/docs/releases' / (tag + '.md')
        if release_notes.is_file():
            notes = release_notes.read_text(encoding='utf-8')
        with Workspace(directory.parent) as work:
            note_file = work.root / 'notes.txt'
            note_file.write_text(notes)
            work.run(['gh', 'release', 'create', tag, '--repo', repo, '--verify-tag', '--draft', '--target', commit,
                      '--title', 'Chain Panel ' + tag, '--notes-file', note_file])
    run(['gh', 'release', 'upload', tag, '--repo', repo, '--clobber', *sorted(directory.iterdir())])
    # Download and compare every remote byte before making the release public.
    with Workspace(directory.parent) as work:
        downloaded = work.root / 'assets'
        downloaded.mkdir()
        work.run(['gh', 'release', 'download', tag, '--repo', repo, '--dir', downloaded])
        require({p.name for p in downloaded.iterdir()} == expected, '草稿附件数量不匹配。')
        for name in expected:
            require(sha256(downloaded / name) == sha256(directory / name), f'上传后校验失败：{name}')
    run(['gh', 'release', 'edit', tag, '--repo', repo, '--draft=false', '--latest'])


def smoke(directory):
    archives = list(directory.glob('*.tar.gz'))
    require(len(archives) == 1, '需要唯一的本机架构发行包。')
    verify_archive_checksum(archives[0])
    with Workspace(directory.parent) as work:
        stage = work.root / 'stage'
        manifest = safe_extract(archives[0], stage)
        restore_modes(stage, manifest)
        work.env['CHAIN_TEST_ROOT'] = str(work.root)
        root = Path(__file__).resolve().parents[1]
        work.run([sys.executable, '-B', root / 'panel/tests/local_install.py', stage], cwd=root)
        work.env.update(CHAIN_TEST_BIN_DIR=str(stage / 'bin'), CHAIN_TEST_XRAY=str(stage / 'bin/xray'))
        work.run([sys.executable, '-B', root / 'panel/tests/legacy_migration.py'], cwd=root)
        work.run([sys.executable, '-B', root / 'panel/tests/customer-plans.py'], cwd=root)
        if shutil.which('nginx') and shutil.which('openssl'):
            work.run([sys.executable, '-B', root / 'panel/tests/https_install.py', stage], cwd=root)
        if manifest['os'] == 'linux' and manifest['arch'] == 'x86_64':
            name = 'chain-openrc-install-' + str(os.getpid())
            for scenario in ['fresh', 'recovery']:
                try:
                    work.run(['docker', 'run', '--rm', '--init', '--name', name,
                        '--mount', f'type=bind,src={root},dst=/source,readonly',
                        '--mount', f'type=bind,src={stage},dst=/release,readonly',
                        '--workdir', '/source', '--env', 'PYTHONDONTWRITEBYTECODE=1', '--env', 'CHAIN_TEST_OPENRC=1',
                        'alpine:3.23.6@sha256:85fe1e81d6758c208f3e1eed4338a1997e19d4be002d4dd32d3100c9a8c010a0',
                        'sh', '-ec', 'apk add --no-cache openrc python3 openssl curl iproute2 nginx certbot; '
                        'exec python3 -B panel/tests/openrc_install.py /release "$1"', 'sh', scenario])
                finally:
                    run(['docker', 'rm', '-f', name], check=False, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)


def check():
    root = Path(__file__).resolve().parents[1]
    with Workspace(Path(os.environ.get('RUNNER_TEMP', str(root.parent)))) as work:
        work.env['PYTHONUNBUFFERED'] = '1'
        log = work.root / 'checks.log'
        try:
            with log.open('w') as stream:
                work.run(['bash', root / 'scripts/version-release.sh', 'check'], cwd=root,
                         stdout=stream, stderr=subprocess.STDOUT)
        except subprocess.CalledProcessError:
            # Make the actionable failure visible in the check annotation even
            # when an unauthenticated maintainer cannot download Actions logs.
            tail = log.read_bytes()[-3000:].decode('utf-8', errors='replace')
            escaped = tail.replace('%', '%25').replace('\r', '%0D').replace('\n', '%0A')
            print('::error title=Installer verification failed::' + escaped, flush=True)
            raise
        finally:
            print(log.read_text(), flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('action', choices=['validate', 'assemble', 'publish', 'smoke', 'check'])
    parser.add_argument('--tag', default=os.environ.get('GITHUB_REF_NAME', ''))
    parser.add_argument('--commit', default=os.environ.get('GITHUB_SHA', ''))
    parser.add_argument('--repo', default=os.environ.get('GITHUB_REPOSITORY', ''))
    parser.add_argument('--input', type=Path)
    parser.add_argument('--output', type=Path)
    args = parser.parse_args()
    if args.action != 'check':
        validate_tag(args.tag)
    if args.action == 'assemble':
        assemble(args.input, args.output, args.tag, args.commit)
    elif args.action == 'publish':
        publish(args.input, args.tag, args.repo, args.commit)
    elif args.action == 'smoke':
        smoke(args.input)
    elif args.action == 'check':
        check()


if __name__ == '__main__':
    signal.signal(signal.SIGTERM, lambda *_: sys.exit(143))
    main()
