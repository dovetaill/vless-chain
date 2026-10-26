"""Pinned core downloads and agent-only upgrades; never regenerate customer configuration."""
import contextlib
import ctypes
import gzip
import json
import os
from pathlib import Path
import pwd
import grp
import re
import shutil
import stat
import subprocess
import sys
import tempfile
import urllib.request
import uuid
import zipfile

from chain_common import (VERSION_RE, architecture, operating_system, require, sha256,
                          json_write, verify_tree, restore_modes)

_versions = Path(__file__).with_name('core-versions.json')
RECOMMENDED = json.loads((_versions if _versions.exists() else Path(__file__).resolve().parents[1] / 'panel/src/core-versions.json').read_text())
REPOS = {'xray': 'XTLS/Xray-core', 'mihomo': 'MetaCubeX/mihomo'}
LIMIT = 128 * 1024**2


def validate(value):
    require(value.get('core') in REPOS, '请选择 Xray 或 Mihomo')
    action = value['action']
    if action == 'core-upgrade':
        require('task_id' not in value, '升级操作不能指定原任务 ID')
        require(isinstance(value.get('version'), str) and re.fullmatch(VERSION_RE, value['version']), '请选择准确内核版本')
        for key in ('security_update', 'rollback_safe', 'prerelease'):
            require(type(value.get(key, False)) is bool, '内核升级选项无效')
        require(not value.get('rollback_safe') or value.get('security_update'), '仅安全更新需要确认回退版本安全')
    elif action in ('core-confirm', 'core-reject'):
        require(not any(k in value for k in ('version', 'security_update', 'rollback_safe', 'prerelease')), '确认操作不能修改升级参数')
        require(str(uuid.UUID(value.get('task_id', ''))) == value['task_id'], '原升级任务 ID 无效')
    elif action != 'core-rollback':
        raise ValueError('不支持的内核操作')
    else:
        require(not any(k in value for k in ('version', 'security_update', 'rollback_safe', 'prerelease', 'task_id')), '回退操作使用已保存的安全版本')


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


def replace_core(stage, manifest, core, source, metadata):
    names = [f'bin/{core}', 'share/' + ('Xray' if core == 'xray' else 'Mihomo') + '-LICENSE']
    for name in names:
        target = stage / name
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(source / name, target)
        mode = 0o755 if name.startswith('bin/') else 0o644
        target.chmod(mode)
        manifest['files'][name] = {'sha256': sha256(target), 'mode': mode}
    if core == 'xray':
        (stage / 'share/Xray-PATCHES.txt').unlink(missing_ok=True)
        manifest['files'].pop('share/Xray-PATCHES.txt', None)
        if (source / 'share/Xray-PATCHES.txt').is_file():
            name = 'share/Xray-PATCHES.txt'
            shutil.copyfile(source / name, stage / name)
            manifest['files'][name] = {'sha256': sha256(stage / name), 'mode': 0o644}
    manifest[core] = metadata['version']
    manifest[core + '_selected'] = True
    for key in ('source', 'archive_sha256', 'prerelease'):
        manifest[core + '_' + key] = metadata.get(key)
    if core == 'xray':
        manifest['xray_channel'] = 'selected'
    json_write(stage / 'release.json', manifest, 0o644)
    verify_tree(stage)


def preserve(manager, stage, manifest):
    """Panel/package updates retain independently selected cores."""
    state = manager.state()
    require(not (manager.base / '.core-pending.json').exists(), '请先完成内核升级验证或恢复，再更新面板')
    require(not state.get('core_blocked'), '安全更新尚未恢复代理，请先安装安全内核')
    current = state.get('roles', {}).get('agent')
    if not current:
        return
    source = manager.base / 'releases' / current['current']
    old = verify_tree(source)
    for core in REPOS:
        if old.get(core + '_selected'):
            replace_core(stage, manifest, core, source, {'version': old[core], **{
                key: old.get(core + '_' + key) for key in ('source', 'archive_sha256', 'prerelease')}})


def confined(directory):
    """Landlock permits validation writes only inside the disposable worker directory."""
    require(sys.platform == 'linux', '内核隔离预检目前需要 Linux')
    libc = ctypes.CDLL(None, use_errno=True)
    abi = libc.syscall(444, 0, 0, 1)
    require(abi >= 3, '内核预检需要 Linux Landlock ABI 3+；未运行候选程序或切换版本')
    access = sum(1 << bit for bit in (1, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14))
    class Ruleset(ctypes.Structure):
        _fields_ = [('handled_access_fs', ctypes.c_uint64)]
    class PathRule(ctypes.Structure):
        _pack_ = 1
        _fields_ = [('allowed_access', ctypes.c_uint64), ('parent_fd', ctypes.c_int32)]
    ruleset = Ruleset(access)
    fd = libc.syscall(444, ctypes.byref(ruleset), ctypes.sizeof(ruleset), 0)
    require(fd >= 0, '无法创建只写临时目录的预检隔离')
    try:
        parent = os.open(directory, os.O_PATH | os.O_CLOEXEC)
        try:
            rule = PathRule(access, parent)
            require(libc.syscall(445, fd, 1, ctypes.byref(rule), 0) == 0, '无法限定预检写入路径')
        finally:
            os.close(parent)
        require(libc.prctl(38, 1, 0, 0, 0) == 0 and libc.syscall(446, fd, 0) == 0, '无法启用预检隔离')
    finally:
        os.close(fd)


def worker():
    # Executed after dropping to the actual Agent service identity, before any core initialization.
    directory = Path(sys.argv[2]).resolve(strict=True)
    confined(directory)
    # Landlock does not restrict chmod/chown; seccomp also denies metadata changes and networking.
    deny_metadata_and_network()
    import resource
    resource.setrlimit(resource.RLIMIT_FSIZE, (8 * 1024**2, 8 * 1024**2))
    request = json.loads((directory / 'request.json').read_text())
    os.execve(request['args'][0], request['args'], request['env'])


def deny_metadata_and_network():
    import platform
    import errno
    if platform.machine() == 'x86_64':
        arch = 0xC000003E
        denied = [41, 42, 43, 49, 50, 53, 90, 91, 92, 93, 94, 132, 188, 189, 190, 197, 198, 199, 235, 260, 261, 268, 280, 288, 437, 452]
    elif platform.machine() in ('aarch64', 'arm64'):
        arch = 0xC00000B7
        denied = [5, 6, 7, 14, 15, 16, 52, 53, 54, 55, 88, 198, 199, 200, 201, 202, 203, 242, 437, 452]
    else:
        raise ValueError('不支持的隔离架构')
    class Filter(ctypes.Structure):
        _fields_ = [('code', ctypes.c_ushort), ('jt', ctypes.c_ubyte), ('jf', ctypes.c_ubyte), ('k', ctypes.c_uint32)]
    class Program(ctypes.Structure):
        _fields_ = [('len', ctypes.c_ushort), ('filter', ctypes.POINTER(Filter))]
    instructions = [(0x20, 0, 0, 4), (0x15, 1, 0, arch), (0x06, 0, 0, 0x80000000), (0x20, 0, 0, 0)]
    if arch == 0xC000003E:
        instructions += [(0x45, 0, 1, 0x40000000), (0x06, 0, 0, 0x80000000)]
    for call in denied:
        instructions += [(0x15, 0, 1, call), (0x06, 0, 0, 0x00050000 | errno.EPERM)]
    instructions.append((0x06, 0, 0, 0x7fff0000))
    filters = (Filter * len(instructions))(*(Filter(*i) for i in instructions))
    program = Program(len(instructions), filters)
    libc = ctypes.CDLL(None, use_errno=True)
    require(libc.prctl(38, 1, 0, 0, 0) == 0 and libc.prctl(22, 2, ctypes.byref(program), 0, 0) == 0, '无法禁止预检修改权限和访问网络')


def service_identity(manager, env):
    unit = manager.role_paths('agent')[2]
    from chain_install import OWNER
    from chain_platform import service_kind
    require(not unit.is_symlink() and OWNER in unit.read_text(), 'Agent 服务归属不匹配')
    if not manager.live:
        return os.geteuid(), os.getegid(), os.getgroups()
    if service_kind() == 'systemd':
        result = subprocess.run(['systemctl', 'show', 'chain-agent.service', '--property=User,Group,SupplementaryGroups,MainPID'],
                                check=True, capture_output=True, text=True, timeout=15)
        values = dict(line.split('=', 1) for line in result.stdout.splitlines() if '=' in line)
        account = pwd.getpwnam(values['User'])
        gid = grp.getgrnam(values['Group']).gr_gid if values.get('Group') else account.pw_gid
        groups = sorted(set(os.getgrouplist(account.pw_name, gid)) |
                        {grp.getgrnam(g).gr_gid for g in values.get('SupplementaryGroups', '').split()})
    else:
        import shlex
        line = next((line for line in unit.read_text().splitlines() if line.startswith('command_user=')), '')
        user, group = shlex.split(line.split('=', 1)[-1])[0].split(':')
        account, gid = pwd.getpwnam(user), grp.getgrnam(group).gr_gid
        groups = os.getgrouplist(user, gid)
    require(account.pw_uid != 0, '拒绝以 root 运行候选内核')
    data = Path(env['CHAIN_AGENT_DIR'])
    require(data.stat().st_uid == account.pw_uid and data.stat().st_gid == gid, 'Agent 数据目录与实际服务身份不匹配，未修改权限')
    # Verify all existing workers of the installed Agent, including service overrides.
    binary = (manager.base / 'current-agent/bin/chain-agent').resolve(strict=True)
    for process in Path('/proc').iterdir():
        if not process.name.isdigit():
            continue
        try:
            if (process / 'exe').resolve() != binary:
                continue
            fields = dict(line.split(':', 1) for line in (process / 'status').read_text().splitlines() if ':' in line)
            require(int(fields['Uid'].split()[1]) == account.pw_uid and int(fields['Gid'].split()[1]) == gid
                    and set(map(int, fields['Groups'].split())) == set(groups), 'Agent 实际进程身份与服务定义不一致')
        except (FileNotFoundError, PermissionError):
            continue
    return account.pw_uid, gid, groups


def configs(env, core):
    data = Path(env['CHAIN_AGENT_DIR'])
    active = data / 'active.json'
    if not active.exists():
        return []
    desired = json.loads(active.read_text())['config']
    if core == 'mihomo':
        return [(data / 'mihomo/config.json', '')] if desired.get('_mihomo') is not None else []
    result = [(data / 'xray.json', '')]
    for profile in desired.get('_legacy', []):
        ident = str(uuid.UUID(profile['id']))
        require(ident == profile['id'], '旧配置 ID 无效')
        result.append((data / ('legacy-' + ident) / 'config.json', profile.get('asset_dir', '')))
    return result


def snapshot(env, core):
    paths = [Path(env['CHAIN_AGENT_DIR']) / 'active.json', *(p for p, _ in configs(env, core))]
    return {str(p): sha256(p) if p.exists() else None for p in paths}


def preflight(manager, env, core, binary, version):
    uid, gid, groups = service_identity(manager, env)
    with tempfile.TemporaryDirectory(prefix='chain-core-check-', dir=manager.path('/var/tmp')) as temporary:
        work = Path(temporary)
        target = work / core
        shutil.copyfile(binary, target)
        target.chmod(0o755)
        if core == 'mihomo':
            for name in ('GeoSite.dat', 'GeoIP.dat', 'Country.mmdb', 'ASN.mmdb', 'geoip.metadb'):
                asset_path = Path(env['CHAIN_AGENT_DIR']) / 'mihomo' / name
                if asset_path.is_file() and not asset_path.is_symlink():
                    shutil.copyfile(asset_path, work / name)
        checks = [([str(target), 'version' if core == 'xray' else '-v'], None, '')]
        for index, (path, assets) in enumerate(configs(env, core)):
            require(path.is_file() and not path.is_symlink(), '现用内核配置缺失或类型无效')
            value = json.loads(path.read_text())
            if core == 'xray':
                value['log'] = {'loglevel': 'none'}
            candidate = work / f'config-{index}.json'
            json_write(candidate, value)
            args = [str(target), 'run', '-test', '-config', str(candidate)] if core == 'xray' else [
                str(target), '-t', '-f', str(candidate), '-d', str(work)]
            checks.append((args, candidate, assets))
        if os.geteuid() == 0:
            for path in [work, *work.iterdir()]:
                os.chown(path, uid, gid)
        for args, config, assets in checks:
            child_env = {'PATH': '/usr/bin:/bin', 'HOME': str(work), 'TMPDIR': str(work),
                         'SAFE_PATHS': str(Path(env['CHAIN_AGENT_DIR']) / 'acme/config'), 'XRAY_LOCATION_ASSET': assets or str(work)}
            request = work / 'request.json'
            json_write(request, {'args': args, 'env': child_env})
            if os.geteuid() == 0:
                os.chown(request, uid, gid)
            kwargs = {'user': uid, 'group': gid, 'extra_groups': groups} if os.geteuid() == 0 else {}
            with (work / 'check.log').open('w+b') as log:
                process = subprocess.Popen([sys.executable, '-B', str(Path(__file__).resolve()), '--worker', str(work)],
                                           cwd=work, env={**child_env, 'PYTHONDONTWRITEBYTECODE': '1'},
                                           stdout=log, stderr=subprocess.STDOUT, start_new_session=True, **kwargs)
                try:
                    code = process.wait(timeout=30)
                finally:
                    import signal
                    with contextlib.suppress(ProcessLookupError):
                        os.killpg(process.pid, signal.SIGKILL)
                    process.wait()
                log.seek(0)
                text = log.read(65536).decode(errors='replace')
            require(code == 0, f'{core} 隔离预检未通过（版本或配置 {config.name if config else "version"}）；现用配置保持原样')
            if config is None:
                require(re.search(r'(?<![\d.])v?' + re.escape(version.split('-')[0]) + r'(?![\d.])', text), '候选内核实际版本与目标不匹配')


def status(manager):
    state = manager.state()
    current = state.get('roles', {}).get('agent')
    if not current:
        return {'supported': False}
    source = manager.base / 'releases' / current['current']
    manifest = json.loads((source / 'release.json').read_text())
    pending = manager.base / '.core-pending.json'
    return {'supported': True, 'versions': {core: manifest.get(core) for core in REPOS},
            'recommended': RECOMMENDED, 'history': state.get('core_history', {}),
            'blocked': state.get('core_blocked'), 'pending': json.loads(pending.read_text()).get('task_id') if pending.exists() else None}


def block(manager, state, core, task_id, env):
    marker = Path(env['CHAIN_AGENT_DIR']) / '.core-upgrade-blocked'
    if marker.exists():
        require(not marker.is_symlink() and json.loads(marker.read_text()).get('owner') == 'chain-core-upgrade-v1', '代理停用标记归属不匹配')
    json_write(marker, {'owner': 'chain-core-upgrade-v1', 'core': core, 'task_id': task_id})
    state['core_blocked'] = {'core': core, 'task_id': task_id}
    json_write(manager.state_file, state)
    manager.system.call('stop', 'chain-agent.service')


def unblock(env):
    marker = Path(env['CHAIN_AGENT_DIR']) / '.core-upgrade-blocked'
    if marker.exists():
        require(not marker.is_symlink() and json.loads(marker.read_text()).get('owner') == 'chain-core-upgrade-v1', '代理停用标记归属不匹配')
        marker.unlink()


def recover(manager, journal):
    state = manager.state()
    manager.system.call('stop', 'chain-agent.service')
    if journal['rollback_allowed'] and snapshot(journal['env'], journal['core']) == journal['config_snapshot']:
        old = journal['before']
        source = manager.base / 'releases' / old['current']
        verify_tree(source)
        manager.link('agent', old['current'])
        state['roles']['agent'] = old
        json_write(manager.state_file, state)
        if journal['was_running']:
            manager.system.call('start', 'chain-agent.service')
            manager.healthy('agent', journal['env'])
        outcome = '已恢复原内核；配置与节点身份保持原样'
    else:
        block(manager, state, journal['core'], journal['task_id'], journal['env'])
        outcome = '已停止本机代理，管理面板保留；没有确认可用且安全的回退版本，请安装安全版本'
    (manager.base / '.core-pending.json').unlink()
    return outcome


def operate(manager, job, directory):
    from chain_install import parse_env, OWNER
    core, action = job['core'], job['action']
    journal_path = manager.base / '.core-pending.json'
    with manager.lock():
        if action in ('core-confirm', 'core-reject'):
            require(journal_path.is_file() and not journal_path.is_symlink(), '没有待确认的内核升级')
            journal = json.loads(journal_path.read_text())
            require(journal['task_id'] == job['task_id'] and journal['core'] == core, '待确认任务不匹配')
            state = manager.state()
            current = state['roles']['agent']['current']
            allowed = {journal['target']} if action == 'core-confirm' else {journal['target'], journal['before']['current']}
            require(current in allowed and (manager.base / 'current-agent').resolve() in {
                (manager.base / 'releases' / name).resolve() for name in allowed}, '当前版本已变化，拒绝覆盖')
            original = Path(directory) / (job['task_id'] + '.json')
            receipt = json.loads(original.read_text())
            if action == 'core-reject':
                detail = recover(manager, journal)
            else:
                require(snapshot(journal['env'], core) == journal['config_snapshot'], '配置已变化，请重新验证，未确认旧测试结果')
                manager.healthy('agent', journal['env'])
                state.setdefault('core_history', {})[core] = {
                    'release': journal['before']['current'], 'version': journal['previous_version'],
                    'safe': journal['rollback_allowed']}
                state.pop('core_blocked', None)
                json_write(manager.state_file, state)
                journal_path.unlink()
                detail = '内核版本及实际连接已确认；UUID、密钥和原配置保持原样'
            receipt.update(state='succeeded' if action == 'core-confirm' else 'failed', detail=detail)
            receipt.pop('error', None)
            json_write(original, receipt)
            manager.prune_releases(manager.state())
            return {'state': 'succeeded', 'detail': detail}
        require(not journal_path.exists(), '请先确认或恢复上次内核升级')
        state = manager.state()
        require('agent' in state['roles'], '本机尚未安装 Agent')
        require(not state.get('core_blocked') or state['core_blocked']['core'] == core, '请先恢复被停用的内核')
        before = state['roles']['agent'].copy()
        require(re.fullmatch(r'[A-Za-z0-9_.-]+', before['current']) and before['current'] not in ('.', '..'), '当前发行目录无效')
        source = manager.base / 'releases' / before['current']
        require((manager.base / 'current-agent').is_symlink() and (manager.base / 'current-agent').resolve() == source.resolve(), 'Agent 程序链接与安装记录不一致')
        manifest = verify_tree(source)
        env = parse_env(manager.role_paths('agent')[0] / 'agent.env')
        require(Path(env.get('CHAIN_XRAY_BIN', '')).resolve() == (source / 'bin/xray').resolve()
                and not env.get('CHAIN_MIHOMO_BIN'), '自定义内核路径不支持自动替换，请先核对部署')
        require((Path(env['CHAIN_AGENT_DIR']) / '.chain-owner').read_text() == 'chain-agent', 'Agent 数据目录归属不匹配')
        baseline = snapshot(env, core)
        security = job.get('security_update', False)
        rollback_allowed = not security or job.get('rollback_safe', False)
        with tempfile.TemporaryDirectory(prefix='work-' + job['id'] + '-', dir=directory) as temporary:
            work = Path(temporary)
            stage = work / 'stage'
            shutil.copytree(source, stage)
            if action == 'core-rollback':
                require(not state.get('core_blocked'), '安全更新停用后必须明确选择安全版本，历史安全确认已不适用')
                history = state.get('core_history', {}).get(core)
                require(history and history.get('safe'), '没有确认安全的上一版；请选择明确的安全版本升级')
                require(re.fullmatch(r'[A-Za-z0-9_.-]+', history['release']) and history['release'] not in ('.', '..'), '回退版本记录无效')
                core_source = manager.base / 'releases' / history['release']
                prior = verify_tree(core_source)
                metadata = {'version': prior[core], **{key: prior.get(core + '_' + key) for key in ('source', 'archive_sha256', 'prerelease')}}
            else:
                core_source = work / 'download'
                core_source.mkdir()
                try:
                    metadata = download(core, job['version'], core_source, core_source / 'bin' / core, job.get('prerelease', False))
                except Exception:
                    if security and not rollback_allowed:
                        block(manager, state, core, job['id'], env)
                    raise
            try:
                preflight(manager, env, core, core_source / 'bin' / core, metadata['version'])
                require(snapshot(env, core) == baseline, '预检期间配置已变化，未切换内核')
            except Exception:
                if security and not rollback_allowed:
                    block(manager, state, core, job['id'], env)
                raise
            replace_core(stage, manifest, core, core_source, metadata)
            release = f'{manifest["version"]}-{manifest.get("os", "linux")}-{manifest["arch"]}-{sha256(stage / "release.json")[:12]}'
            destination = manager.base / 'releases' / release
            if destination.exists():
                require(verify_tree(destination) == manifest, '目标发行目录冲突')
            else:
                restore_modes(stage, manifest)
                shutil.copytree(stage, destination)
            was_running = manager.system.active('agent')
            journal = {'task_id': job['id'], 'core': core, 'before': before, 'target': release, 'env': env,
                       'config_snapshot': baseline, 'previous_version': verify_tree(source).get(core),
                       'rollback_allowed': rollback_allowed and not state.get('core_blocked'), 'was_running': was_running}
            # The journal also protects rollback releases from routine package pruning.
            json_write(journal_path, journal)
            try:
                manager.system.call('stop', 'chain-agent.service')
                manager.link('agent', release)
                state['roles']['agent']['current'] = release
                state.pop('core_blocked', None)
                json_write(manager.state_file, state)
                unblock(env)
                manager.system.call('start', 'chain-agent.service')
                manager.healthy('agent', journal['env'])
            except Exception:
                detail = recover(manager, journal)
                raise ValueError('内核切换失败；' + detail) from None
    return {'state': 'awaiting_verification', 'core_version': metadata['version'],
            'detail': '目标内核版本和配置预检通过；请用原 VLESS 链接验证实际传输，确认前不会继续下一台'}


if __name__ == '__main__' and sys.argv[1:2] == ['--worker']:
    worker()
