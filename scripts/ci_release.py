"""Validate both native builds and publish a complete, immutable Release."""
import argparse
import json
import os
from pathlib import Path
import re
import shutil
import signal
import sys

from chain_common import VERSION_RE, Workspace, atomic_write, output, require, restore_modes, run, safe_extract, sha256, verify_archive_checksum
from chain_release import version

ARCHES = ('x86_64', 'aarch64')


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
        for arch in ARCHES:
            directory = source / ('release-' + arch)
            name = f'chain-panel-{tag[1:]}-linux-{arch}.tar.gz'
            archive = directory / name
            verify_archive_checksum(archive)
            manifest = safe_extract(archive, work.root / arch)
            require(manifest == json.loads((directory / 'release.json').read_text()), '包内外清单不一致。')
            require(manifest['version'] == tag[1:] and manifest['arch'] == arch
                    and manifest['target'] == f'{arch}-unknown-linux-gnu'
                    and manifest['commit'] == commit and manifest['dirty'] is False,
                    '架构、版本或提交来源不一致，拒绝发布。')
            require(tuple(map(int, manifest['glibc_min'].split('.'))) <= (2, 35), '构建环境提高了 glibc 最低要求。')
            shutil.copy2(archive, final / name)
            shutil.copy2(directory / 'release.json', final / f'release-linux-{arch}.json')
            sums.append(f'{sha256(archive)}  {name}\n')
        atomic_write(final / 'SHA256SUMS', ''.join(sums), 0o644)
        shutil.copytree(final, destination)


def publish(directory, tag, repo, commit):
    validate_tag(tag)
    require(re.fullmatch(r'[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+', repo), '仓库名称无效。')
    expected = {'SHA256SUMS'} | {f'chain-panel-{tag[1:]}-linux-{a}.tar.gz' for a in ARCHES} | {
        f'release-linux-{a}.json' for a in ARCHES}
    require({p.name for p in directory.iterdir()} == expected, '发布附件不完整。')
    for arch in ARCHES:
        verify_archive_checksum(directory / f'chain-panel-{tag[1:]}-linux-{arch}.tar.gz')
    releases = json.loads(output(['gh', 'api', f'repos/{repo}/releases?per_page=100']))
    existing = next((item for item in releases if item['tag_name'] == tag), None)
    if existing:
        require(existing['draft'], '此版本已经正式发布，不覆盖已有附件。')
        require(existing['target_commitish'] == commit, '已有草稿来自其他提交，拒绝覆盖。')
    else:
        notes = ('一条命令安装控制台与本机节点：\n\n'
                 '```bash\ncurl -fsSL https://raw.githubusercontent.com/' + repo +
                 '/main/scripts/install-server.sh | sudo bash\n```\n\n'
                 '首次只需输入可选域名。自动读取网卡公网 IPv4、选择空闲随机端口、生成登录令牌。'
                 '填写域名后检查 A/AAAA 解析，通过系统包管理器安装 Nginx/Certbot，配置 HTTPS 与自动续期。\n\n'
                 '支持 Ubuntu 22.04+ / Debian 12+，Linux x86_64 与 aarch64，systemd、glibc ≥ 2.35、Python ≥ 3.10。'
                 '下载对应架构的 tar.gz 与 SHA256SUMS 可离线安装。')
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
        if shutil.which('nginx') and shutil.which('openssl'):
            work.run([sys.executable, '-B', root / 'panel/tests/https_install.py', stage], cwd=root)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('action', choices=['validate', 'assemble', 'publish', 'smoke'])
    parser.add_argument('--tag', default=os.environ.get('GITHUB_REF_NAME', ''))
    parser.add_argument('--commit', default=os.environ.get('GITHUB_SHA', ''))
    parser.add_argument('--repo', default=os.environ.get('GITHUB_REPOSITORY', ''))
    parser.add_argument('--input', type=Path)
    parser.add_argument('--output', type=Path)
    args = parser.parse_args()
    validate_tag(args.tag)
    if args.action == 'assemble':
        assemble(args.input, args.output, args.tag, args.commit)
    elif args.action == 'publish':
        publish(args.input, args.tag, args.repo, args.commit)
    elif args.action == 'smoke':
        smoke(args.input)


if __name__ == '__main__':
    signal.signal(signal.SIGTERM, lambda *_: sys.exit(143))
    main()
