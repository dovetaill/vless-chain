"""Linux/macOS release builder; invoked through version-release.sh."""
import argparse
import contextlib
import datetime
import hashlib
import gzip
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

from chain_common import (Workspace, architecture, operating_system, atomic_write, json_write, output, require,
                          run, sha256, verify_archive_checksum, VERSION_RE)

ROOT = Path(__file__).resolve().parents[1]
XRAY_VERSION = '26.3.27-chain.1'
XRAY_SOURCES = (
    ('github.com/xtls/xray-core@v1.260327.0', 'h1:g4TzxMwyPrxslZh6uD+FiG3lXKTrnNO+b4ky2OhogHE='),
    ('github.com/xtls/reality@v0.0.0-20260322125925-9234c772ba8f', 'h1:iy2JRioxmUpoJ3SzbFPyTxHZMbR/rSHP7dOOgYaq1O8='),
)
MIHOMO_VERSION = '1.19.32'
MIHOMO_ASSETS = {
    ('linux', 'x86_64'): ('amd64-compatible', 'ba3ce607747a07f948fc35780e108a4a7c7f552a38b9bd4d115f313ebcb89c20'),
    ('linux', 'aarch64'): ('arm64', '9dd862e28b46ff7d775f169cceebc28deccaa0a9e804237d421cd2571e0caba0'),
    ('darwin', 'x86_64'): ('amd64-compatible', '18b382df77bded2ad0fb3db27db5636cb15b20729d5ba995a357eeb9b46bf507'),
    ('darwin', 'aarch64'): ('arm64', '3312a6780652c622890fd4357c6a853bbf865464fd047ac7b7f52dab8de18652'),
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


def push_release(tag):
    print(f'将推送 main 和 {tag}，触发版本构建。')
    answer = input('现在推送？[Y/n]（回车推送，n 仅保留本地）: ').strip().lower()
    require(answer in ('', 'y', 'yes', 'n', 'no'), '未识别的选项，未推送；本地提交与标签保留。')
    if answer in ('n', 'no'):
        print(f'{tag} 已保留在本地，尚未推送。稍后选择菜单 10「单独推送已有版本」。')
        return
    run(['git', 'push', '--atomic', 'origin', 'main', 'refs/tags/' + tag], cwd=ROOT)
    print(f'{tag} 已推送；等待 Actions 构建完成并公开正式 Release 后，再运行 sudo chainctl update。')


def release_wizard(push_only=False):
    require(sys.stdin.isatty(), '请在终端打开发布向导，确认变更后再推送。')
    paths = [*SHIPPED, '.github']
    def git(*args):
        return output(['git', *args], cwd=ROOT)
    require(git('branch', '--show-current') == 'main', '请切换到 main 分支后发布。')
    require(not git('diff', '--cached', '--name-only'), '暂存区已有改动，请先提交或取消暂存后重试；向导不会改动已有暂存内容。')
    require(not git('ls-files', '-u'), '请先解决 Git 合并冲突。')
    status = git('status', '--porcelain', '--untracked-files=all', '--', *paths)
    require(not status, '请先提交发行源码，再运行发布向导；以下文件尚未提交：\n' + status)
    remote = git('remote', 'get-url', '--push', 'origin')
    require(remote == git('remote', 'get-url', 'origin'), 'origin 的拉取与推送地址不同，请先统一地址，确保检查的是发布目标。')
    print(f'发布目标：{remote}；分支：main', flush=True)
    run(['git', 'fetch', 'origin', 'main'], cwd=ROOT)
    run(['git', 'merge-base', '--is-ancestor', 'FETCH_HEAD', 'HEAD'], cwd=ROOT)
    old = version()
    current_tag = 'v' + old
    local_tags = git('tag', '--list').splitlines()
    remote_tags = git('ls-remote', '--tags', 'origin')
    remote_names = {line.split('\t')[1] for line in remote_tags.splitlines()}
    # Resume a failed push without creating another version or rewriting a commit.
    if push_only or (current_tag in local_tags and f'refs/tags/{current_tag}' not in remote_names):
        require(current_tag in local_tags, f'本地没有 {current_tag}，请先通过菜单 9 创建版本。')
        require(git('rev-parse', current_tag + '^{commit}') == git('rev-parse', 'HEAD'),
                f'{current_tag} 未指向当前提交，请先处理版本与提交的对应关系；不会自动移动标签。')
        print(f'推送已有版本 {current_tag}，不递增版本或创建提交。')
        push_release(current_tag)
        return
    major, minor, patch = map(int, old.split('.'))
    suggested = f'{major}.{minor}.{patch + 1}'
    value = input(f'下一版本 [{suggested}]（回车自动递增）: ').strip() or suggested
    require(re.fullmatch(VERSION_RE, value), '版本须为 X.Y.Z。')
    require(tuple(map(int, value.split('.'))) > (major, minor, patch), '新版本必须大于当前版本。')
    tag = 'v' + value
    require(tag not in local_tags and f'refs/tags/{tag}' not in remote_names, '该标签已经存在，请使用新版本。')
    manifests = ['panel/Cargo.toml', 'panel/Cargo.lock', 'panel/frontend/package.json', 'panel/frontend/package-lock.json']
    print('\n仅同步并提交版本文件：\n' + '\n'.join(manifests))
    print('\n一并推送的已有提交：')
    run(['git', '--no-pager', 'log', '--oneline', 'FETCH_HEAD..HEAD'], cwd=ROOT)
    message = (f'chore: 发布 {tag}\n\n背景与目的：为已提交的业务代码创建正式版本，供安装器获取。\n\n'
               f'具体修改：同步 Cargo/npm 清单及锁文件，从 {old} 升级到 {value}。\n'
               '影响范围：仅四份版本文件，不修改业务逻辑；业务变更说明和验证记录见此前提交。\n\n'
               '验证：通过 Git 差异空白、四份版本一致性、远端历史及标签冲突检查。\n'
               '本次仅修改版本元数据，不重复运行业务测试；完整构建和安装验证由发布 CI 执行，结果以 CI 为准。\n\n'
               '发布方式：创建附注标签，随后可选择原子推送 main 和标签以触发 Actions；不会升级运行中的服务。\n')
    print('\n' + message)
    if input(f'确认创建版本提交和标签 {tag}？输入 y: ').strip().lower() != 'y':
        print('已取消，未修改版本、提交或推送。')
        return
    try:
        set_version(value)
        run(['git', 'add', '--', *manifests], cwd=ROOT)
        run(['git', 'diff', '--cached', '--check'], cwd=ROOT)
        require(version() == value, '版本校验失败。')
        run(['git', 'commit', '-F', '-'], cwd=ROOT, input=message, text=True)
        run(['git', 'tag', '-a', tag, '-m', f'发布 {tag}：同步版本文件并发布已提交的业务代码。'], cwd=ROOT)
        push_release(tag)
    except (ValueError, OSError, subprocess.CalledProcessError):
        print('发布未完成，已产生的版本改动、提交及标签均保留，不自动回退或覆盖。\n'
              '若仅推送失败，重新进入此向导可继续推送；其他失败请先检查 git status。', file=sys.stderr)
        raise


def fetch_mihomo(work, destination):
    arch, digest = MIHOMO_ASSETS[operating_system(), architecture()]
    name = f'mihomo-{operating_system()}-{arch}-v{MIHOMO_VERSION}.gz'
    archive = work.root / name
    url = f'https://github.com/MetaCubeX/mihomo/releases/download/v{MIHOMO_VERSION}/{name}'
    with urllib.request.urlopen(url, timeout=60) as response, archive.open('wb') as stream:
        shutil.copyfileobj(response, stream)
    require(sha256(archive) == digest, 'Mihomo 下载校验失败。')
    destination.parent.mkdir(parents=True, exist_ok=True)
    with gzip.open(archive, 'rb') as source, destination.open('wb') as output:
        shutil.copyfileobj(source, output)
    destination.chmod(0o755)
    license_path = destination.parent.parent / 'share/Mihomo-LICENSE'
    license_path.parent.mkdir(parents=True, exist_ok=True)
    with urllib.request.urlopen(f'https://raw.githubusercontent.com/MetaCubeX/mihomo/v{MIHOMO_VERSION}/LICENSE', timeout=30) as response:
        license_path.write_bytes(response.read())


def fetch_xray(work, destination):
    require(shutil.which('go'), '构建补丁内核需要 Go；安装后的服务器无需 Go。')
    work.env.update(GOTOOLCHAIN='go1.26.4', GOENV='off', GOFLAGS='-modcacherw', CGO_ENABLED='0',
                    GOPATH=str(work.root / 'go'), GOMODCACHE=str(work.root / 'go/mod'),
                    GOCACHE=str(work.root / 'go/cache'), GOPROXY='https://proxy.golang.org', GOSUMDB='sum.golang.org')
    try:
        sources = []
        for index, (module, digest) in enumerate(XRAY_SOURCES):
            metadata = work.root / f'xray-source-{index}.json'
            with metadata.open('wb') as stream:
                work.run(['go', 'mod', 'download', '-json', module], cwd=work.root, stdout=stream)
            info = json.loads(metadata.read_text())
            require(info.get('Sum') == digest and not info.get('Error'), 'Xray 源码校验失败。')
            source = work.root / f'xray-source-{index}'
            shutil.copytree(info['Dir'], source, copy_function=shutil.copyfile)
            source.chmod(0o700)
            for directory in source.rglob('*'):
                if directory.is_dir():
                    directory.chmod(0o700)
            sources.append(source)
        core, reality = sources
        file = reality / 'tls.go'
        original = file.read_text()
        require(original.count('\tsize  = 8192\n') == 1, 'REALITY 补丁基线不匹配，拒绝构建。')
        # Backport only XTLS/REALITY#33; retain the stable baseline's client compatibility.
        file.write_text(original.replace('\tsize  = 8192\n', '\tsize  = 17 * 1024\n'))
        work.run(['go', 'mod', 'edit', '-replace', f'github.com/xtls/reality={reality}'], cwd=core)
        destination.parent.mkdir(parents=True, exist_ok=True)
        work.run(['go', 'build', '-mod=readonly', '-trimpath', '-buildvcs=false', '-ldflags',
                  '-s -w -buildid= -X github.com/xtls/xray-core/core.build=chain.1-reality-buffer-17k',
                  '-o', destination, './main'], cwd=core)
        share = destination.parent.parent / 'share'
        share.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(core / 'LICENSE', share / 'Xray-LICENSE')
        (share / 'Xray-PATCHES.txt').write_text(
            f'{XRAY_VERSION}: 本项目补丁构建，并非官方原版二进制。\n'
            '基线：Xray v26.3.27；仅回移 https://github.com/XTLS/REALITY/pull/33\n'
            'tls.go: size = 8192 -> size = 17 * 1024；保留原 REALITY 客户端兼容性。\n'
            'Go 1.26.4；源码模块及校验：\n' + '\n'.join(f'{m} {s}' for m, s in XRAY_SOURCES) + '\n')
    finally:
        # Go toolchain downloads stay read-only even with -modcacherw.
        work.env['GOTOOLCHAIN'] = 'local'
        work.run(['go', 'clean', '-modcache'], cwd=work.root)


def default_output():
    return ROOT / 'release-output' / f'v{version()}-{operating_system()}-{architecture()}'


def configure_ci_cache(env):
    # Only hosted workflow builds retain caches; local Workspace cleanup is unchanged.
    if os.environ.get('GITHUB_ACTIONS') == 'true' and os.environ.get('CHAIN_BUILD_CACHE') == '1':
        cache = Path(os.environ['RUNNER_TEMP']).resolve() / 'chain-build-cache'
        env.update(CARGO_TARGET_DIR=str(cache / 'target'),
                   CARGO_HOME=str(cache / 'cargo'), npm_config_cache=str(cache / 'npm'))


def check(args):
    run(['bash', '-n', ROOT / 'scripts/version-release.sh', ROOT / 'scripts/install-server.sh'])
    run([sys.executable, '-B', ROOT / 'scripts/tests/test_delivery.py'])
    run([sys.executable, '-B', ROOT / 'scripts/tests/test_network.py'])
    run([sys.executable, '-B', ROOT / 'scripts/tests/test_legacy.py'])
    run([sys.executable, '-B', ROOT / 'scripts/tests/test_platform.py'])
    run([sys.executable, '-B', ROOT / 'scripts/tests/test_ingress.py'])
    run([sys.executable, '-B', ROOT / 'scripts/tests/test_maintenance.py'])
    run([sys.executable, '-B', ROOT / 'scripts/tests/test_time_diagnostics.py'])
    run([sys.executable, '-B', ROOT / 'scripts/tests/test_menu.py'])
    if args.full:
        with Workspace(args.work_parent or ROOT.parent) as work:
            xray = work.root / 'bin/xray'
            fetch_xray(work, xray)
            command = [sys.executable, '-B', ROOT / 'panel/scripts/verify.py', '--xray', xray, '--work-parent', work.root]
            if args.browser:
                command.append('--browser')
            work.run(command, cwd=ROOT / 'panel')
    else:
        require(not args.browser, '--browser 需要配合 --full。')
    print('所选检查已完成。' if args.full else '脚本检查已完成；完整前后端与链路验证使用 check --full。')


def build(args):
    for name in ['node', 'npm', 'cargo', 'rustc', 'cc', 'git']:
        require(shutil.which(name), f'缺少 {name}；请先安装构建依赖，脚本不会自动修改系统工具链。')
    node = tuple(int(v) for v in output(['node', '--version']).lstrip('v').split('.')[:3])
    require((node[0] == 20 and node >= (20, 19, 0)) or node >= (22, 12, 0), 'Vite 需要 Node.js 20.19+ 或 22.12+。')
    arch, ver = architecture(), version()
    target = f'{arch}-unknown-linux-musl' if operating_system() == 'linux' else f'{arch}-apple-darwin'
    require(target in output(['rustup', 'target', 'list', '--installed']).splitlines(), f'请先安装 Rust 构建目标：rustup target add {target}')
    if operating_system() == 'linux':
        require(shutil.which('musl-gcc'), 'Linux 静态发行包需要 musl-gcc（musl-tools）和 Perl。')
        require(shutil.which('readelf'), 'Linux 静态发行包需要 readelf（binutils）验证链接结果。')
    destination = Path(args.output or default_output()).resolve()
    require(not destination.exists(), f'输出目录已存在：{destination}；请使用新的 --output 目录，避免覆盖已发行文件。')
    dirty = bool(source_status())
    commit = output(['git', 'rev-parse', 'HEAD'], cwd=ROOT)
    with Workspace(args.work_parent or ROOT.parent) as work:
        configure_ci_cache(work.env)
        project = work.root / 'project'
        project.mkdir()
        for name in ['Cargo.toml', 'Cargo.lock', 'build.rs']:
            shutil.copy2(ROOT / 'panel' / name, project / name)
        for name in ['src', 'empty-web', 'frontend', 'tests/fixtures']:
            shutil.copytree(ROOT / 'panel' / name, project / name,
                            ignore=shutil.ignore_patterns('node_modules', 'dist', '.env', '.env.*', '__pycache__'))
        work.env['CHAIN_WEB_ASSETS'] = str(project / 'frontend/dist')
        if operating_system() == 'linux':
            # Older musl-gcc linker specs break static PIE; Rust supplies its own musl runtime.
            work.env['CC_' + target.replace('-', '_')] = 'musl-gcc'
        else:
            work.env['MACOSX_DEPLOYMENT_TARGET'] = '14.0'
        work.run(['npm', 'ci', '--no-audit', '--no-fund'], cwd=project / 'frontend')
        work.run(['npm', 'run', 'build'], cwd=project / 'frontend')
        work.run(['cargo', 'test', '--locked', '--lib', '--quiet'], cwd=project)
        work.run(['cargo', 'test', '--locked', '--bin', 'chain-panel', 'latest_xray', '--quiet'], cwd=project)
        work.run(['cargo', 'build', '--locked', '--release', '--bins', '--target', target], cwd=project)
        stage = work.root / 'stage'
        (stage / 'bin').mkdir(parents=True)
        for binary in ['chain-panel', 'chain-agent']:
            shutil.copy2(Path(work.env['CARGO_TARGET_DIR']) / target / 'release' / binary, stage / 'bin' / binary)
            if operating_system() == 'linux':
                headers = output(['readelf', '-l', '-d', stage / 'bin' / binary])
                require('INTERP' not in headers and '(NEEDED)' not in headers, f'{binary} 不是完全静态发行程序。')
        fetch_xray(work, stage / 'bin/xray')
        work.run([stage / 'bin/xray', 'version'])
        work.env.update(CHAIN_TEST_ROOT=str(work.root), CHAIN_TEST_XRAY=str(stage / 'bin/xray'))
        work.run(['node', ROOT / 'panel/tests/xray-reality.cjs'], cwd=ROOT)
        fetch_mihomo(work, stage / 'bin/mihomo')
        work.run([stage / 'bin/mihomo', '-v'])
        for name in ['install-server.sh', 'chain_install.py', 'chain_common.py', 'chain_network.py', 'chain_ingress.py', 'chain_platform.py', 'chain_legacy.py', 'chain_maintenance.py', 'chain_time.py']:
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
        manifest = {'format': 1, 'version': ver, 'arch': arch, 'os': operating_system(), 'target': target,
                    'commit': commit, 'dirty': dirty, 'xray': XRAY_VERSION, 'xray_channel': 'recommended', 'mihomo': MIHOMO_VERSION, 'files': files,
                    'created_at': datetime.datetime.now(datetime.timezone.utc).isoformat()}
        manifest.update({'libc': 'musl'} if operating_system() == 'linux' else {'macos_min': '14.0'})
        json_write(stage / 'release.json', manifest, 0o644)
        name = f'chain-panel-{ver}-{operating_system()}-{arch}.tar.gz'
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
          f'运行要求：{operating_system()} {arch}；包含固定版本代理内核，无需 Node.js/Rust。')
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
    name = f'chain-panel-{manifest["version"]}-{manifest.get("os", "linux")}-{manifest["arch"]}.tar.gz'
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
              '6) 创建 GitHub Release 草稿\n7) 发布已有 GitHub 草稿\n8) 本机安装与运维\n'
              '9) 发布已提交的代码（默认：递增版本、提交版本文件、打标签，可选推送）\n'
              '10) 单独推送已有版本（不递增版本）\n0) 退出')
        choice = input('选择 [9]: ').strip() or '9'
        if choice == '0':
            return
        command = {'1': ['build'], '2': ['check'], '3': ['check', '--full'], '4': ['status'], '8': ['ops'], '9': ['release'], '10': ['push']}.get(choice)
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
        else:
            item.add_argument('--directory', type=Path)
            item.add_argument('--repo', required=True)
    sub.add_parser('status')
    sub.add_parser('release', help='发布已提交的代码：递增版本、提交版本文件、打标签并推送')
    sub.add_parser('push', help='单独推送当前已有版本，不递增版本或创建提交')
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
    elif args.action == 'release':
        release_wizard()
    elif args.action == 'push':
        release_wizard(push_only=True)
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
    except (ValueError, OSError, subprocess.CalledProcessError, KeyboardInterrupt, EOFError) as error:
        raise SystemExit(str(error) or '操作已中断。')
