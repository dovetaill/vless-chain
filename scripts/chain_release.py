"""Native Linux release builder; invoked through version-release.sh."""
import argparse
import contextlib
import datetime
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import signal
import subprocess
import sys
import tarfile
import urllib.request
import zipfile

from chain_common import (Workspace, architecture, atomic_write, json_write, output, require,
                          run, sha256, verify_archive_checksum, VERSION_RE)

ROOT = Path(__file__).resolve().parents[1]
XRAY_VERSION = '26.3.27'
# Pinned official release ZIP checksums (GitHub asset digest); no unauthenticated latest download.
XRAY_ASSETS = {
    'x86_64': ('Xray-linux-64.zip', '23cd9af937744d97776ee35ecad4972cf4b2109d1e0fe6be9930467608f7c8ae'),
    'aarch64': ('Xray-linux-arm64-v8a.zip', '4d30283ae614e3057f730f67cd088a42be6fdf91f8639d82cb69e48cde80413c'),
}
SHIPPED = ['panel', 'scripts', 'README.md', '.gitignore']


def version():
    cargo = (ROOT / 'panel/Cargo.toml').read_text()
    value = re.search(r'^version = "([^"]+)"$', cargo, re.M).group(1)
    require(re.fullmatch(VERSION_RE, value), '版本须为 X.Y.Z。')
    package = json.loads((ROOT / 'panel/frontend/package.json').read_text())
    lock = json.loads((ROOT / 'panel/frontend/package-lock.json').read_text())
    cargo_lock = (ROOT / 'panel/Cargo.lock').read_text()
    require(package['version'] == lock['version'] == lock['packages']['']['version'] == value
            and f'name = "chain-panel"\nversion = "{value}"' in cargo_lock,
            'Cargo、npm 清单与锁文件版本不一致；执行 version --set X.Y.Z 同步。')
    return value


def set_version(value):
    require(re.fullmatch(VERSION_RE, value), '版本须为 X.Y.Z。')
    changes = {}
    cargo = ROOT / 'panel/Cargo.toml'
    changes[cargo] = re.sub(r'^version = "[^"]+"$', f'version = "{value}"', cargo.read_text(), count=1, flags=re.M)
    lock = ROOT / 'panel/Cargo.lock'
    text, count = re.subn(r'(name = "chain-panel"\nversion = ")[^"]+("\n)',
                          lambda m: m[1] + value + m[2], lock.read_text())
    require(count == 1, 'Cargo.lock 缺少唯一的 chain-panel 包。')
    changes[lock] = text
    for name in ['package.json', 'package-lock.json']:
        path = ROOT / 'panel/frontend' / name
        document = json.loads(path.read_text())
        document['version'] = value
        if name == 'package-lock.json':
            document['packages']['']['version'] = value
        changes[path] = json.dumps(document, ensure_ascii=False, indent=2) + '\n'
    for path, text in changes.items():
        atomic_write(path, text, 0o644)
    print(f'版本已设为 {version()}。请检查并提交版本变更后再发布到 GitHub。')


def source_status():
    return output(['git', 'status', '--porcelain', '--untracked-files=normal', '--', *SHIPPED], cwd=ROOT)


def fetch_xray(work, destination):
    name, digest = XRAY_ASSETS[architecture()]
    archive = work.root / name
    url = f'https://github.com/XTLS/Xray-core/releases/download/v{XRAY_VERSION}/{name}'
    print(f'下载固定版本 Xray v{XRAY_VERSION} 并校验 SHA256。', flush=True)
    with urllib.request.urlopen(url, timeout=60) as response, archive.open('wb') as stream:
        shutil.copyfileobj(response, stream)
    require(sha256(archive) == digest, 'Xray ZIP 校验失败。')
    with zipfile.ZipFile(archive) as bundle:
        destination.parent.mkdir(parents=True, exist_ok=True)
        destination.write_bytes(bundle.read('xray'))
        license_path = destination.parent.parent / 'share/Xray-LICENSE'
        license_path.parent.mkdir(parents=True, exist_ok=True)
        license_path.write_bytes(bundle.read('LICENSE'))
    destination.chmod(0o755)


def default_output():
    return ROOT / 'release-output' / f'v{version()}-linux-{architecture()}'


def check(args):
    run(['bash', '-n', ROOT / 'scripts/version-release.sh', ROOT / 'scripts/install-server.sh'])
    run([sys.executable, '-B', ROOT / 'scripts/tests/test_delivery.py'])
    if args.full:
        with Workspace(args.work_parent or ROOT.parent) as work:
            xray = work.root / 'bin/xray'
            fetch_xray(work, xray)
            command = [sys.executable, '-B', ROOT / 'panel/scripts/verify.py', '--xray', xray, '--work-parent', work.root]
            if args.browser:
                command.append('--browser')
            if args.ssh:
                command.append('--ssh')
            work.run(command, cwd=ROOT / 'panel')
    else:
        require(not args.browser, '--browser 需要配合 --full。')
        require(not args.ssh, '--ssh 需要配合 --full。')
    print('所选检查已完成。' if args.full else '脚本检查已完成；完整前后端与链路验证使用 check --full。')


def build(args):
    for name in ['node', 'npm', 'cargo', 'rustc', 'cc', 'git']:
        require(shutil.which(name), f'缺少 {name}；请先安装构建依赖，脚本不会自动修改系统工具链。')
    node = tuple(int(v) for v in output(['node', '--version']).lstrip('v').split('.')[:3])
    require((node[0] == 20 and node >= (20, 19, 0)) or node >= (22, 12, 0), 'Vite 需要 Node.js 20.19+ 或 22.12+。')
    arch, ver = architecture(), version()
    target = re.search(r'^host: (.+)$', output(['rustc', '-vV']), re.M).group(1)
    require(target == f'{arch}-unknown-linux-gnu', '首版只构建本机 GNU/Linux 目标，不执行交叉编译。')
    destination = Path(args.output or default_output()).resolve()
    require(not destination.exists(), f'输出目录已存在：{destination}；请使用新的 --output 目录，避免覆盖已发行文件。')
    dirty = bool(source_status())
    commit = output(['git', 'rev-parse', 'HEAD'], cwd=ROOT)
    with Workspace(args.work_parent or ROOT.parent) as work:
        project = work.root / 'project'
        project.mkdir()
        for name in ['Cargo.toml', 'Cargo.lock', 'build.rs']:
            shutil.copy2(ROOT / 'panel' / name, project / name)
        for name in ['src', 'empty-web', 'frontend']:
            shutil.copytree(ROOT / 'panel' / name, project / name,
                            ignore=shutil.ignore_patterns('node_modules', 'dist', '.env', '.env.*', '__pycache__'))
        work.env['CHAIN_WEB_ASSETS'] = str(project / 'frontend/dist')
        work.run(['npm', 'ci', '--no-audit', '--no-fund'], cwd=project / 'frontend')
        work.run(['npm', 'run', 'build'], cwd=project / 'frontend')
        work.run(['cargo', 'test', '--locked', '--lib', '--quiet'], cwd=project)
        work.run(['cargo', 'build', '--locked', '--release', '--bins', '--target', target], cwd=project)
        stage = work.root / 'stage'
        (stage / 'bin').mkdir(parents=True)
        for binary in ['chain-panel', 'chain-agent']:
            shutil.copy2(work.root / 'target' / target / 'release' / binary, stage / 'bin' / binary)
        fetch_xray(work, stage / 'bin/xray')
        work.run([stage / 'bin/xray', 'version'])
        for name in ['install-server.sh', 'chain_install.py', 'chain_common.py']:
            (stage / 'scripts').mkdir(exist_ok=True)
            shutil.copy2(ROOT / 'scripts' / name, stage / 'scripts' / name)
        shutil.copy2(ROOT / 'panel/README.md', stage / 'README.md')
        shutil.copytree(ROOT / 'panel/docs', stage / 'docs')
        (stage / 'frontend').mkdir()
        shutil.copy2(ROOT / 'panel/frontend/THEME.md', stage / 'frontend/THEME.md')
        shutil.copy2(ROOT / 'panel/frontend/THEME.md', stage / 'THEME.md')
        files = {}
        for file in sorted(stage.rglob('*')):
            if file.is_file():
                name = str(file.relative_to(stage))
                mode = 0o755 if name.startswith('bin/') or name.endswith('.sh') else 0o644
                file.chmod(mode)
                files[name] = {'sha256': sha256(file), 'mode': mode}
        glibc = output(['getconf', 'GNU_LIBC_VERSION']).split()[-1]
        manifest = {'format': 1, 'version': ver, 'arch': arch, 'target': target, 'glibc_min': glibc,
                    'commit': commit, 'dirty': dirty, 'xray': XRAY_VERSION, 'files': files,
                    'created_at': datetime.datetime.now(datetime.timezone.utc).isoformat()}
        json_write(stage / 'release.json', manifest, 0o644)
        name = f'chain-panel-{ver}-linux-{arch}.tar.gz'
        package = work.root / name
        with tarfile.open(package, 'w:gz') as bundle:
            for file in sorted(stage.rglob('*')):
                if file.is_file():
                    info = bundle.gettarinfo(str(file), arcname=str(file.relative_to(stage)))
                    info.uid = info.gid = 0
                    info.uname = info.gname = 'root'
                    with file.open('rb') as stream:
                        bundle.addfile(info, stream)
        # Copy only final deliverables; dependencies, sources and target remain task-owned.
        destination.mkdir(parents=True)
        shutil.copy2(package, destination / name)
        shutil.copy2(stage / 'release.json', destination / 'release.json')
        atomic_write(destination / 'SHA256SUMS', f'{sha256(package)}  {name}\n', 0o644)
        verify_archive_checksum(destination / name)
    print(f'发行包：{destination / name}\n校验文件：{destination / "SHA256SUMS"}\n'
          f'运行要求：Linux {arch}，glibc ≥ {glibc}；包含固定版本 Xray，无需 Node.js/Rust。')
    if dirty:
        print('这是包含未提交修改的本地构建；可本地安装，GitHub 发布前需提交并重新构建。')


def github(args):
    require(shutil.which('gh'), '需要 GitHub CLI（gh），并先执行 gh auth login。')
    require(re.fullmatch(r'[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+', args.repo or ''), '请使用 --repo OWNER/REPO 明确发布仓库。')
    require(not source_status(), '发行输入存在未提交修改；请先提交 panel/、scripts/ 和相关文档。')
    directory = Path(args.directory or default_output()).resolve(strict=True)
    manifest = json.loads((directory / 'release.json').read_text())
    require(not manifest['dirty'] and manifest['version'] == version()
            and manifest['commit'] == output(['git', 'rev-parse', 'HEAD'], cwd=ROOT),
            '发行包必须来自当前版本对应的干净提交。')
    tag = 'v' + manifest['version']
    name = f'chain-panel-{manifest["version"]}-linux-{manifest["arch"]}.tar.gz'
    verify_archive_checksum(directory / name)
    run(['gh', 'api', f'repos/{args.repo}/commits/{manifest["commit"]}', '--silent'])
    if args.action == 'draft':
        run(['gh', 'release', 'create', tag, '--repo', args.repo, '--draft', '--target', manifest['commit'],
             '--title', f'Chain Panel {tag}', '--notes', 'Rust 多节点控制台与 Agent；下载对应架构发行包及 SHA256SUMS，参见包内 README。',
             directory / name, directory / 'SHA256SUMS', directory / 'release.json'])
        print('GitHub Release 草稿已创建；使用 publish 显式发布。')
    else:
        release = json.loads(output(['gh', 'release', 'view', tag, '--repo', args.repo, '--json', 'isDraft,assets']))
        require(release['isDraft'], '只发布已有草稿，不修改已发布版本。')
        require({name, 'release.json', 'SHA256SUMS'}.issubset(a['name'] for a in release['assets']), '草稿附件不完整。')
        with Workspace(args.work_parent or ROOT.parent) as work:
            work.run(['gh', 'release', 'download', tag, '--repo', args.repo, '--dir', work.root,
                      '--pattern', name, '--pattern', 'release.json', '--pattern', 'SHA256SUMS'])
            require((work.root / 'release.json').read_bytes() == (directory / 'release.json').read_bytes(), '草稿清单与本地发行包不一致。')
            require(verify_archive_checksum(work.root / name) == sha256(directory / name), '草稿附件与本地发行包不一致。')
        run(['gh', 'release', 'edit', tag, '--repo', args.repo, '--draft=false', '--latest'])


def menu():
    require(sys.stdin.isatty(), '非交互环境请指定 build / check / status / version / draft / publish / ops，或 --help。')
    while True:
        print(f'\nChain Panel v{version()} · 构建与发布\n'
              '1) 构建发行包（含前端、Rust 程序与 Xray）\n2) 检查脚本与安装流程\n'
              '3) 完整前后端和真实链路验证\n4) 查看版本与 Git 状态\n5) 设置下一版本\n'
              '6) 创建 GitHub Release 草稿\n7) 发布已有 GitHub 草稿\n8) 本机安装与运维\n0) 退出')
        choice = input('选择 [1]: ').strip() or '1'
        if choice == '0':
            return
        command = {'1': ['build'], '2': ['check'], '3': ['check', '--full'], '4': ['status'], '8': ['ops']}.get(choice)
        if choice == '5':
            command = ['version', '--set', input('新版本 X.Y.Z: ').strip()]
        if choice in ('6', '7'):
            command = ['draft' if choice == '6' else 'publish', '--repo', input('GitHub 仓库 OWNER/REPO: ').strip()]
        if command:
            try:
                main(command)
            except (ValueError, OSError, subprocess.CalledProcessError) as error:
                print(f'操作未完成：{error}', file=sys.stderr)


def main(argv=None):
    parser = argparse.ArgumentParser(description='Chain Panel 构建、版本与发布入口；无需在仓库根目录执行 npm ci。')
    sub = parser.add_subparsers(dest='action')
    sub.add_parser('menu', help='交互菜单')
    for action in ['build', 'check', 'draft', 'publish']:
        item = sub.add_parser(action)
        item.add_argument('--work-parent', type=Path, help='临时构建目录所在的大容量磁盘；默认仓库父目录')
        if action == 'build':
            item.add_argument('--output', type=Path, help='新的最终发行目录')
        elif action == 'check':
            item.add_argument('--full', action='store_true')
            item.add_argument('--browser', action='store_true')
            item.add_argument('--ssh', action='store_true', help='隔离 SSH 传包验证，需要 root 与 sshd')
        else:
            item.add_argument('--directory', type=Path)
            item.add_argument('--repo', required=True)
    sub.add_parser('status')
    sub.add_parser('version').add_argument('--set')
    sub.add_parser('ops').add_argument('arguments', nargs=argparse.REMAINDER)
    args = parser.parse_args(argv)
    if args.action in (None, 'menu'):
        return menu()
    if args.action == 'build':
        build(args)
    elif args.action == 'check':
        check(args)
    elif args.action == 'version':
        set_version(vars(args)['set']) if vars(args)['set'] else print(version())
    elif args.action == 'status':
        print(f'版本：{version()}\n架构：{architecture()}\n默认发行目录：{default_output()}')
        run(['git', 'status', '--short'], cwd=ROOT)
    elif args.action == 'ops':
        run(['bash', ROOT / 'scripts/install-server.sh', *args.arguments])
    else:
        github(args)


if __name__ == '__main__':
    signal.signal(signal.SIGTERM, lambda *_: sys.exit(143))
    try:
        main()
    except (ValueError, OSError, subprocess.CalledProcessError, KeyboardInterrupt) as error:
        raise SystemExit(str(error) or '操作已中断。')
