"""Discover and hand over explicitly selected proxy services; never kill by name."""
import contextlib
import hashlib
import ipaddress
import json
import os
from pathlib import Path
import re
import shutil
import sqlite3
import subprocess
import time
import urllib.request
import uuid

from chain_common import atomic_write, json_write, output, require, run

PROXY = re.compile(r'^(?:xray(?:[-_].*)?|v2ray(?:[-_].*)?|x-ui|3x-ui|sing-box|ss-server|shadowsocks.*)$', re.I)


def json_config(raw):
    # Strip JSON comments without changing URLs or quoted strings.
    clean = re.sub(r'"(?:\\.|[^"\\])*"|//[^\n]*|/\*[\s\S]*?\*/',
                   lambda m: m[0] if m[0].startswith('"') else ' ', raw)
    return json.loads(clean)


def read_config(path, limit=2 * 1024**2):
    require(path.is_file() and path.stat().st_size <= limit, f'配置或依赖文件缺失或超过 {limit // 1024**2} MiB。')
    return path.read_bytes()


def configs(command, cwd):
    result = []
    for index, arg in enumerate(command):
        key, equal, value = arg.partition('=')
        if key in ('-c', '-config', '--config', '-confdir', '--confdir'):
            value = value if equal else command[index + 1] if index + 1 < len(command) else ''
            if value:
                path = Path(value)
                result.append((path if path.is_absolute() else cwd / path, 'confdir' in key))
    return result


def openrc_owner(process):
    proc = process.parent
    with contextlib.suppress(OSError, ValueError, UnicodeError):
        if (process / 'ns/pid').readlink() != (proc / '1/ns/pid').readlink():
            return None
        status = dict(line.split(':', 1) for line in (process / 'status').read_text().splitlines())
        parent = proc / status.get('PPid', '0').strip()
        binary = shutil.which('supervise-daemon')
        if not binary or (parent / 'exe').readlink() != Path(binary).resolve():
            return None
        command = (parent / 'cmdline').read_bytes().decode().rstrip('\0').split('\0')
        env = dict(line.split('=', 1) for line in (parent / 'environ').read_bytes().decode().split('\0') if '=' in line)
        name = env.get('RC_SVCNAME', '')
        if (not re.fullmatch(r'[A-Za-z0-9][A-Za-z0-9_.-]*', name) or name.startswith('chain-')
                or command[1:3] != [name, '--start'] or env.get('RC_SERVICE') != '/etc/init.d/' + name):
            return None
        if (process / 'root/etc/init.d' / name).is_file():
            return name
    return None


def discover(proc=Path('/proc')):
    if not proc.is_dir():
        return []
    containers = []
    if shutil.which('docker'):
        probe = run(['docker', 'ps', '-q', '--no-trunc'], check=False, capture_output=True, text=True)
        if probe.returncode == 0 and probe.stdout.strip():
            containers = json.loads(output(['docker', 'inspect', *probe.stdout.split()]))
    namespaces = {}
    for container in containers:
        with contextlib.suppress(OSError):
            namespaces.setdefault((proc / str(container['State']['Pid']) / 'ns/pid').readlink(), []).append(container)
    owners = {}
    for process in sorted(proc.glob('[0-9]*')):
        try:
            command = process.joinpath('cmdline').read_bytes().decode().rstrip('\0').split('\0')
            executable = process.joinpath('exe').readlink()
            if not PROXY.fullmatch(executable.name.removesuffix(' (deleted)')):
                continue
            cgroup = process.joinpath('cgroup').read_text()
            container = next((c for c in containers if c['Id'] in cgroup), None)
            namespace = process.joinpath('ns/pid').readlink()
            matches = namespaces.get(namespace, [])
            if not container and len(matches) == 1 and namespace != (proc / '1/ns/pid').readlink():
                container = matches[0]
            units = re.findall(r'/([^/\n]+\.service)(?:/|$)', cgroup)
            unit = units[-1] if units else None
            if container:
                main = (proc / str(container['State']['Pid']) / 'exe').readlink().name
                image = container.get('Config', {}).get('Image', '')
                if not PROXY.fullmatch(main) and not re.search(r'(?:^|/)(?:xray|v2ray|3x-ui|x-ui)(?:[-:/]|$)', image):
                    container = None
                    unit = None
            elif unit and not unit.startswith(('chain-', 'docker.', 'containerd.')):
                main_pid = output(['systemctl', 'show', unit, '--property=MainPID', '--value'], check=False)
                # A manually launched proxy inside sshd or another application is not its owner.
                if not main_pid.isdigit() or not PROXY.fullmatch((proc / main_pid / 'exe').readlink().name):
                    unit = None
            if container:
                identity, kind = container['Id'], 'docker'
                title = container['Name'].lstrip('/')
            elif unit and not unit.startswith(('chain-', 'docker.', 'containerd.')):
                identity, kind, title = unit, 'systemd', unit
            else:
                name = openrc_owner(process) if not unit else None
                identity, kind, title = (name, 'openrc', name) if name else (process.name, 'unmanaged', executable.name)
            key = kind + ':' + identity
            owner = owners.setdefault(key, {'id': identity, 'kind': kind, 'name': title, 'pids': [],
                                            'files': {}, 'sources': [], 'container': container})
            owner['pids'].append(int(process.name))
            owner['is_xui'] = owner.get('is_xui', False) or bool(re.search(r'(?:3?x-ui)', title + executable.name))
            root = process / 'root'
            if kind == 'openrc':
                for logical in ['/etc/init.d/' + identity, '/etc/conf.d/' + identity]:
                    path = root / logical.lstrip('/')
                    if path.is_file():
                        owner['files'][logical] = {'path': str(path), 'body': read_config(path)}
            cwd = process.joinpath('cwd').readlink()
            paths = configs(command, cwd)
            if not paths and 'x-ui' in executable.name:
                paths = [(executable.parent / 'bin/config.json', False)]
            for logical, directory in paths:
                mapped = root / str(logical).lstrip('/')
                files = sorted(mapped.glob('*.json')) if directory else [mapped]
                for path in files:
                    relative = str(logical / path.name) if directory else str(logical)
                    source_id = key + ':' + relative
                    if any(s['id'] == source_id for s in owner['sources']):
                        continue
                    source = {'id': source_id, 'name': title, 'config': None, 'error': ''}
                    try:
                        body = read_config(path)
                        owner['files'][relative] = {'path': str(path), 'body': body}
                        config = json_config(body.decode())
                        require(isinstance(config, dict), '配置根节点不是对象。')
                        source['config'] = config
                        if directory or len(paths) > 1:
                            source['error'] = '多文件组合配置需手动配置。'
                        hydrate(config, root, cwd, owner)
                        source['original_ports'] = [i.get('port') for i in config.get('inbounds', [])]
                        asset_dir = executable.parent
                        with contextlib.suppress(OSError, UnicodeError):
                            for item in (process / 'environ').read_bytes().decode().split('\0'):
                                if item.startswith('XRAY_LOCATION_ASSET='):
                                    asset_dir = Path(item.partition('=')[2])
                        source['asset_files'] = collect_assets(config, root, cwd, asset_dir, owner)
                        if container and container['HostConfig']['NetworkMode'] != 'host':
                            published_ports(config, container)
                    except (OSError, ValueError, KeyError, TypeError) as error:
                        source['error'] = f'配置读取或依赖检查失败：{error}'
                    owner['sources'].append(source)
            # Keep connection settings, but never activate disabled users or import account/expiry rules.
            db = root / 'etc/x-ui/x-ui.db'
            if db.is_file():
                with contextlib.suppress(sqlite3.Error, OSError):
                    with contextlib.closing(sqlite3.connect(db.as_uri() + '?mode=ro', uri=True)) as connection:
                        rows = connection.execute('SELECT port, remark FROM inbounds').fetchall()
                        try:
                            originals = xui_inbounds(connection)
                            database_error = ''
                        except (ValueError, TypeError) as error:
                            originals, database_error = [], f'x-ui 入口配置读取失败：{error}'
                    names = dict(rows)
                    present = {port for s in owner['sources'] if not s['error'] and s['config']
                               for port in s.get('original_ports', []) if isinstance(port, int)}
                    missing = [i for i in originals if i['enable'] and i['port'] not in present]
                    for source in owner['sources']:
                        if source['error'] or not source['config']:
                            continue
                        for index, inbound in enumerate(source['config'].get('inbounds', [])):
                            port = source['original_ports'][index]
                            if isinstance(port, int) and names.get(port):
                                inbound['remark'] = names[port]
                        source['source_inbounds'] = originals
                        if database_error or missing:
                            source['error'] = database_error or ('原面板存在未包含在 Xray 运行配置中的启用入口（'
                                + '、'.join(str(i['protocol']) for i in missing)
                                + '）；可能使用面板内置内核或受限用户，不能自动停用原面板。')
                    owner['database'] = str(db)
        except (OSError, UnicodeError, ValueError):
            continue
    for owner in owners.values():
        if owner.get('is_xui') and not owner.get('database'):
            for source in owner['sources']:
                source['error'] = source['error'] or '无法读取原面板入口数据库，不能确认是否有非 Xray 服务；保留原面板，不能自动接管。'
        if not owner['sources']:
            owner['sources'] = [{'id': owner['kind'] + ':' + owner['id'], 'name': owner['name'],
                                 'config': None, 'error': '未找到可读取的 Xray 配置，请手动配置。'}]
    return list(owners.values())


def xui_inbounds(connection):
    columns = {row[1] for row in connection.execute('PRAGMA table_info(inbounds)')}
    fields = ['port', 'protocol', 'enable', 'remark', 'settings', 'stream_settings', 'sniffing', 'listen', 'tag']
    if not set(fields) <= columns:
        return []
    query = 'SELECT ' + ','.join(fields) + ' FROM inbounds'
    if 'node_id' in columns:
        query += ' WHERE node_id IS NULL OR node_id = 0'
    rows = connection.execute(query).fetchmany(1025)
    require(len(rows) <= 1024, '原面板入口超过 1024 条。')
    result = []
    for row in rows:
        item = dict(zip(fields, row))
        for field in ['settings', 'stream_settings', 'sniffing']:
            value = item[field]
            item[field] = json.loads(value) if isinstance(value, str) and value.strip() else None
        item['streamSettings'] = item.pop('stream_settings')
        item['enable'] = bool(item['enable'])
        result.append(item)
    return result


def hydrate(config, root, cwd, owner):
    inbounds = config.get('inbounds', [])
    require(isinstance(inbounds, list), 'inbounds 必须是数组。')
    outbounds = config.get('outbounds') or []
    require(isinstance(outbounds, list), 'outbounds 必须是数组。')
    for inbound in inbounds + outbounds:
        require(isinstance(inbound, dict), 'inbounds 条目必须是对象。')
        stream = inbound.get('streamSettings')
        if stream is None:
            continue
        require(isinstance(stream, dict), 'streamSettings 必须是对象或 null。')
        tls = stream.get('tlsSettings')
        if tls is None:
            continue
        require(isinstance(tls, dict), 'tlsSettings 必须是对象或 null。')
        certs = tls.get('certificates')
        if certs is None:
            continue
        require(isinstance(certs, list), 'certificates 必须是数组或 null。')
        for certificate in certs:
            require(isinstance(certificate, dict), 'TLS 证书条目必须是对象。')
            for key, field in [('certificateFile', 'certificate'), ('keyFile', 'key')]:
                if key not in certificate:
                    continue
                require(isinstance(certificate[key], str) and certificate[key].strip(),
                        f'{key} 必须是非空文件路径。')
                logical = Path(certificate[key])
                if not logical.is_absolute():
                    logical = cwd / logical
                path = root / str(logical).lstrip('/')
                body = read_config(path)
                owner['files'][str(logical)] = {'path': str(path), 'body': body}
                certificate[field] = body.decode().splitlines()
                del certificate[key]


def published_ports(config, container):
    network = container.get('NetworkSettings')
    require(isinstance(network, dict), '容器网络信息不可用，请重新扫描。')
    ports = network.get('Ports')
    require(isinstance(ports, dict), '容器端口映射不可用。')
    for inbound in config.get('inbounds', []):
        if inbound.get('tag') == 'api':
            continue
        kinds = ['tcp', 'udp'] if inbound.get('protocol') == 'shadowsocks' else ['tcp']
        mappings = [ports.get(f'{inbound.get("port")}/{kind}') or [] for kind in kinds]
        require(all(mappings), '容器入口没有完整的公网端口映射。')
        require(all(isinstance(group, list) and all(isinstance(p, dict) for p in group) for group in mappings),
                '容器端口映射格式无效。')
        host_ports = {int(p['HostPort']) for group in mappings for p in group}
        require(len(host_ports) == 1 and all(p['HostIp'] in ('', '0.0.0.0', '::') for group in mappings for p in group),
                '容器入口使用特定地址或不同 TCP/UDP 端口，需手动配置。')
        inbound['port'] = host_ports.pop()


def collect_assets(config, root, cwd, asset_dir, owner):
    names = set()
    def visit(value, field=''):
        if isinstance(value, dict):
            for key, item in value.items():
                if field == 'hosts':
                    visit(key, 'domain')
                    visit(item, 'ip')
                else:
                    visit(item, key)
        elif isinstance(value, list):
            for item in value:
                visit(item, field)
        elif isinstance(value, str) and field in ('ip', 'ips', 'domain', 'domains'):
            if value.startswith('geoip:'):
                names.add('geoip.dat')
            elif value.startswith('geosite:'):
                names.add('geosite.dat')
            elif value.startswith(('ext:', 'ext-ip:')):
                name = value.split(':', 2)[1]
                require(re.fullmatch(r'[A-Za-z0-9_.-]+', name) and name not in ('.', '..'),
                        '外部地理数据文件名无效，不能自动迁移。')
                names.add(name)
    visit(config)
    require(len(names) <= 16, '旧配置引用了过多地理数据文件。')
    if not asset_dir.is_absolute():
        asset_dir = cwd / asset_dir
    files = []
    for name in sorted(names):
        logical = asset_dir / name
        path = root / str(logical).lstrip('/')
        require(sum(len(f['body']) for f in owner['files'].values()) + path.stat().st_size <= 128 * 1024**2,
                '旧配置及依赖文件合计超过 128 MiB。')
        body = read_config(path, 64 * 1024**2)
        owner['files'][str(logical)] = {'path': str(path), 'body': body, 'limit': 64 * 1024**2}
        files.append({'name': name, 'logical': str(logical)})
    return files


def stage_assets(manager, owners):
    for owner in owners:
        for source in owner['sources']:
            if source.get('error') or not source.get('asset_files'):
                continue
            files = {item['name']: owner['files'][item['logical']]['body'] for item in source['asset_files']}
            digest = hashlib.sha256()
            for name, body in sorted(files.items()):
                digest.update(name.encode() + b'\0' + hashlib.sha256(body).digest())
            base = manager.path('/var/lib/chain-legacy-assets')
            directory = base / str(uuid.UUID(bytes=digest.digest()[:16]))
            for path in (base, directory):
                if path.exists() or path.is_symlink():
                    marker = path / '.chain-owner'
                    require(path.is_dir() and not path.is_symlink() and marker.is_file()
                            and not marker.is_symlink() and marker.read_text() == 'chain-legacy-assets-v1',
                            '旧配置地理数据目录归属不匹配，拒绝覆盖。')
                else:
                    path.mkdir(parents=True, mode=0o755)
                    path.chmod(0o755)
                    atomic_write(path / '.chain-owner', 'chain-legacy-assets-v1', 0o644)
            for name, body in files.items():
                path = directory / name
                require(not path.is_symlink() and (not path.exists() or path.read_bytes() == body),
                        '旧配置地理数据已变化，拒绝覆盖。')
                if not path.exists():
                    atomic_write(path, body, 0o644)
            source['asset_dir'] = str(directory)


def select(owners, choose):
    selected = []
    for owner in owners:
        print(f'\n发现旧代理服务：{owner["name"]}（{owner["kind"]}；PID {", ".join(map(str, owner["pids"]))}）')
        if owner['kind'] == 'unmanaged':
            print('无法确认其服务管理方式，保留运行；新平台可以选择其他端口。')
            continue
        labels = (owner.get('container') or {}).get('Config', {}).get('Labels') or {}
        if any(k.startswith(('com.docker.swarm.', 'io.kubernetes.')) for k in labels):
            print('此容器由集群调度器管理，保留运行。')
            continue
        choice = choose('如何处理该服务？配置完整导入并通过预检后才会停止旧服务；不兼容时保留旧服务。',
                        {'1': '备份并导入，安装成功后停止旧服务', '2': '备份并停用旧服务，重新配置代理', '3': '保留旧服务'}, '3')
        if choice != '3':
            owner['import'] = choice == '1'
            selected.append(owner)
    return selected


class Handover:
    def __init__(self, selected, backup_root):
        self.selected, self.changed = selected, []
        self.backup = Path(backup_root) / ('legacy-' + time.strftime('%Y%m%dT%H%M%S') + '-' + uuid.uuid4().hex[:8])

    def prepare(self):
        self.backup.mkdir(parents=True, mode=0o700)
        self.backup.chmod(0o700)
        for index, owner in enumerate(self.selected):
            directory = self.backup / str(index)
            directory.mkdir(mode=0o700)
            metadata = {k: v for k, v in owner.items() if k != 'files'}
            if owner['kind'] == 'systemd':
                owner['unit'] = metadata['unit'] = output(['systemctl', 'cat', owner['id']])
                owner['enabled'] = output(['systemctl', 'is-enabled', owner['id']], check=False) == 'enabled'
                owner['active'] = run(['systemctl', 'is-active', '--quiet', owner['id']], check=False).returncode == 0
                metadata.update(enabled=owner['enabled'], active=owner['active'])
            elif owner['kind'] == 'openrc':
                owner['runlevels'] = sorted(p.parent.name for p in Path('/etc/runlevels').glob('*/' + owner['id'])
                                            if p.is_symlink())
                owner['active'] = run(['rc-service', owner['id'], 'status'], check=False, capture_output=True).returncode == 0
                metadata.update(runlevels=owner['runlevels'], active=owner['active'])
            for number, (logical, file) in enumerate(owner['files'].items()):
                require(read_config(Path(file['path']), file.get('limit', 2 * 1024**2)) == file['body'], '旧配置在预检期间已变化，请重新运行安装。')
                atomic_write(directory / f'file-{number}', file['body'])
            metadata['files'] = list(owner['files'])
            if owner.get('database'):
                with contextlib.closing(sqlite3.connect(Path(owner['database']).as_uri() + '?mode=ro', uri=True)) as source, \
                        contextlib.closing(sqlite3.connect(directory / 'x-ui.db')) as destination:
                    source.backup(destination)
                (directory / 'x-ui.db').chmod(0o600)
            json_write(directory / 'source.json', metadata)
        print(f'旧服务配置与恢复信息已备份：{self.backup}')

    def stop(self):
        for owner in self.selected:
            for file in owner['files'].values():
                require(read_config(Path(file['path']), file.get('limit', 2 * 1024**2)) == file['body'], '旧配置在安装期间已变化，取消切换；请重新运行安装。')
            if owner['kind'] == 'systemd' and 'unit' in owner:
                require(output(['systemctl', 'cat', owner['id']]) == owner['unit'], '旧服务定义在安装期间已变化，取消切换。')
            # Record before the first change so a failed stop also restores autostart.
            self.changed.append(owner)
            if owner['kind'] == 'docker':
                run(['docker', 'update', '--restart=no', owner['id']], capture_output=True)
                run(['docker', 'stop', '--time', '30', owner['id']], capture_output=True)
                require(output(['docker', 'inspect', '--format', '{{.State.Running}}', owner['id']]) == 'false', '旧容器未停止。')
            elif owner['kind'] == 'openrc':
                for level in owner['runlevels']:
                    run(['rc-update', 'del', owner['id'], level])
                run(['rc-service', owner['id'], 'stop'])
                require(run(['rc-service', owner['id'], 'status'], check=False, capture_output=True).returncode == 3,
                        '旧 OpenRC 服务未确认停止。')
            else:
                if owner['enabled']:
                    run(['systemctl', 'disable', owner['id']])
                run(['systemctl', 'stop', owner['id']])
                require(run(['systemctl', 'is-active', '--quiet', owner['id']], check=False).returncode != 0, '旧服务未停止。')

    def restore(self):
        failures = []
        for owner in reversed(self.changed):
            try:
                if owner['kind'] == 'docker':
                    policy = owner['container']['HostConfig']['RestartPolicy']
                    restart = policy['Name'] or 'no'
                    if restart == 'on-failure' and policy.get('MaximumRetryCount'):
                        restart += ':' + str(policy['MaximumRetryCount'])
                    run(['docker', 'update', '--restart=' + restart, owner['id']], capture_output=True)
                    run(['docker', 'start', owner['id']], capture_output=True)
                elif owner['kind'] == 'openrc':
                    for level in owner['runlevels']:
                        run(['rc-update', 'add', owner['id'], level])
                    if owner['active']:
                        run(['rc-service', owner['id'], 'start'])
                else:
                    if owner['enabled']:
                        run(['systemctl', 'enable', owner['id']])
                    if owner['active']:
                        run(['systemctl', 'start', owner['id']])
            except Exception as error:
                failures.append(f'{owner["name"]}: {error}')
        require(not failures, f'旧服务恢复未完成：{"；".join(failures)}；备份：{self.backup}')


class PanelAPI:
    def __init__(self, manager):
        from chain_install import parse_env, listen_address
        etc, data, _ = manager.role_paths('panel')
        host, port = listen_address(parse_env(etc / 'panel.env')['CHAIN_LISTEN'])
        host = {'0.0.0.0': '127.0.0.1', '::': '::1'}.get(host, host)
        self.base = f'http://[{host}]:{port}' if ':' in host else f'http://{host}:{port}'
        self.opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
        self.cookie = ''
        self.call('login', {'token': (data / 'admin-token').read_text().strip()})

    def call(self, path, payload=None):
        request = urllib.request.Request(self.base + '/api/' + path, data=None if payload is None else json.dumps(payload).encode(),
                                         headers={'Content-Type': 'application/json', 'X-Chain-Request': '1', 'Cookie': self.cookie})
        with self.opener.open(request, timeout=30) as response:
            if path == 'login':
                self.cookie = response.headers['Set-Cookie'].split(';', 1)[0]
            return json.loads(response.read())

    def wait_ready(self, preserve_config=False):
        deadline = time.monotonic() + 30
        while True:
            state = self.call('state')
            if state['nodes'] and all((n['report'].get('system') or {}).get('gateway_version') == 2
                    and (not preserve_config or (n['report'].get('system') or {}).get('legacy_config_version') == 1)
                    for n in state['nodes']):
                break
            require(time.monotonic() < deadline, '本机 Agent 尚未就绪，旧服务将恢复。')
            time.sleep(.5)

    def publish(self):
        self.wait_ready()
        result = self.call('publish', {})
        deadline = time.monotonic() + 260
        while True:
            job = next((j for j in self.call('state')['jobs'] if j['id'] == result['job_id']), None)
            require(job is not None, '找不到迁移发布任务。')
            if job['state'] == 'applied':
                return
            require(job['state'] in ('publishing', 'rolling_back') and time.monotonic() < deadline,
                    '迁移配置应用失败，旧服务将恢复；请检查 Web 发布记录。')
            time.sleep(.5)


def alternate_port(choose, reserved):
    from chain_network import available_port, listeners
    while True:
        choice = choose('为新平台选择公网 HTTPS 端口，原代理端口保持不变',
                        {'1': '随机空闲端口', '2': '手动指定端口', '0': '取消安装'})
        require(choice != '0', '已取消安装，旧服务保持原样。')
        try:
            port = available_port() if choice == '1' else int(input('新平台 HTTPS 端口: ').strip())
            require(1 <= port <= 65535 and port not in reserved and not listeners((port,)), '端口无效、已占用或被迁移的代理使用。')
            return port
        except ValueError as error:
            print(error)


def manual_certificate(site):
    from chain_network import install_packages
    if not shutil.which('openssl'):
        install_packages(['openssl'])
    while True:
        print('保留旧端口时使用已有证书；请提供与面板域名匹配、由原工具继续续期的宿主机证书路径。')
        try:
            cert = input('证书完整链文件（留空取消）: ').strip()
            require(cert, '已取消证书配置，旧服务保持原样。')
            site.certificate = Path(cert).absolute()
            key = input('证书私钥文件路径: ').strip()
            require(key, '私钥路径不能为空。')
            site.key = Path(key).absolute()
            require(site.certificate_valid(), '证书已过期或不可用。')
            return
        except (OSError, ValueError, subprocess.CalledProcessError) as error:
            if not cert:
                raise
            print(error)


def guided_install(manager, source, manifest, args):
    from chain_install import choose, install_components, listen_address, parse_env, write_env
    from chain_network import HttpsSite, foreign_listeners, listeners, install_proxy_acme, conflicting_site, nginx_site_directory
    from chain_ingress import Ingress

    owners = discover()
    selected = select(owners, choose)
    conflicts = foreign_listeners()
    nginx_conflict = False
    if args.domain and shutil.which('nginx'):
        try:
            dump = output(['nginx', '-T'])
            nginx_conflict = bool(conflicting_site(dump, args.domain, nginx_site_directory(dump) / 'chain-panel.conf'))
        except (ValueError, subprocess.CalledProcessError):
            nginx_conflict = True
    saved = manager.state()
    saved_manual = (saved['roles'].get('panel') or saved.get('retained_roles', {}).get('panel', {})).get('manual_https')
    if not selected and saved_manual:
        return False
    if not selected and (not args.domain or not conflicts and not nginx_conflict):
        return False
    require(not selected or not args.panel_only, '接管旧代理需要本机 Agent，请取消 --panel-only 后重试；旧服务未改变。')
    if conflicts:
        print('现有端口监听：\n' + conflicts)
    args.defer_access = args.defer_proxy_acme = True
    handover = Handover(selected, manager.path('/var/backups/chain-legacy')) if selected else None
    api, checkpoint = None, None
    ingress_created = False
    acme_undo = {}
    nginx_before = None
    try:
        if handover:
            handover.prepare()
            stage_assets(manager, selected)
        # Stage the panel and Agent while the original proxy is still serving.
        install_components(manager, source, manifest, args)
        api = PanelAPI(manager)
        request = None
        preview = {'imported': [], 'pending': []}
        sources = [s for owner in selected if owner['import'] for s in owner['sources']]
        if sources:
            state = api.call('state')
            require(not state['lines'] and not state['pending_imports'] and len(state['nodes']) == 1,
                    '自动接管用于新面板；当前已有代理或待配置项，请在 Web 处理后重试，旧服务未停止。')
            node_id = (manager.role_paths('panel')[1] / 'local-system-node').read_text().strip()
            request = {'node_id': node_id, 'sources': sources, 'preview': True, 'preserve_config': True}
            preview = api.call('legacy-import', request)
            for item in preview['imported']:
                exit_label = ' → 外部 VLESS 代理' if item.get('chain') else ''
                print(f'可导入：{item["name"]}（{item["protocol"]}，端口 {item["port"]}）{exit_label}')
            for item in preview['pending']:
                print(f'无法自动导入：{item["name"]}；{item["reason"]}')
            if preview['pending']:
                print('旧配置尚不能完整导入：保留全部旧服务，不导入或停用任何旧代理；继续完成面板安装。')
                print(f'旧配置备份保留于 {handover.backup}，可在安装完成后手动配置代理。')
                selected, handover, request = [], None, None
                preview = {'imported': [], 'pending': []}
        ports = {item['port'] for item in preview['imported']}
        selected_pids = {pid for owner in selected for pid in owner['pids']}
        for item in preview['imported']:
            if not item.get('reality_target'):
                continue
            target = urllib.parse.urlsplit('tcp://' + item['reality_target'])
            local = target.hostname == 'localhost'
            with contextlib.suppress(ValueError):
                local = local or ipaddress.ip_address(target.hostname).is_loopback
            if local:
                pids = {int(p) for p in re.findall(r'pid=(\d+)', listeners((target.port,)))}
                require(pids and not pids.intersection(selected_pids),
                        'REALITY 本机目标未监听或会随旧代理停止，取消切换；旧服务保持运行。')
        def retained(port):
            for line in foreign_listeners((port,)).splitlines():
                pids = {int(p) for p in re.findall(r'pid=(\d+)', line)}
                if not pids or not pids.issubset(selected_pids):
                    return True
            return False
        sharing = bool(args.domain and 443 in ports and 80 not in ports and not retained(80) and not retained(443)
                       and all(i.get('can_share') and i['sni'] != args.domain for i in preview['imported'] if i['port'] == 443))
        manual = bool(args.domain and (retained(80) or 80 in ports or nginx_conflict))
        site = HttpsSite(args.domain, listen_address(args.listen)[1], getattr(args, 'domain_ipv6', False)) if args.domain else None
        if args.domain and (manual or retained(443) or (443 in ports and not sharing)):
            site.https_port = alternate_port(choose, ports | {listen_address(args.listen)[1]})
            args.public_url = f'https://{args.domain}:{site.https_port}'
        if manual:
            sharing = False
            manual_certificate(site)
        if request:
            api.wait_ready(preserve_config=True)
            request.update(preview=False, shared_443=sharing)
            database = manager.role_paths('panel')[1] / 'panel.db'
            backup_path = handover.backup / 'panel-before-import.db'
            with contextlib.closing(sqlite3.connect(database)) as source_db, contextlib.closing(sqlite3.connect(backup_path)) as copy:
                source_db.backup(copy)
            checkpoint = backup_path
            checkpoint.chmod(0o600)
            result = api.call('legacy-import', request)
            require(len(result['imported']) == len(preview['imported']) and len(result['pending']) == len(preview['pending']),
                    '导入结果与预检不同，旧服务未停止。')
        if handover:
            # Configurations and names have been saved; placeholders do not delay handover.
            handover.stop()
        nginx_before = HttpsSite('rollback.invalid', 0).service_state('nginx')
        with site.activate() if site else contextlib.nullcontext():
            try:
                if sharing:
                    ingress = Ingress()
                    ingress_created = not ingress.state_path.exists()
                    ingress.enable()
                for role in manager.state()['roles']:
                    uid, gid = manager.account(role)
                    if 80 not in ports and not manual:
                        install_proxy_acme(role, uid, gid, undo=acme_undo)
                if request and preview['imported']:
                    api.publish()
                    print('已导入代理的 TLS 证书按手动证书保存；原证书续期后，请在 Web 同步更新。')
                etc, _, _ = manager.role_paths('panel')
                env = parse_env(etc / 'panel.env')
                if env.get('CHAIN_PUBLIC_URL') != args.public_url:
                    env['CHAIN_PUBLIC_URL'] = args.public_url
                    write_env(etc / 'panel.env', env)
                    manager.system.call('restart', 'chain-panel.service')
                    manager.healthy('panel', env)
                state = manager.state()
                state['roles']['panel']['manual_https'] = ({'port': site.https_port, 'cert': str(site.certificate), 'key': str(site.key)} if manual else None)
                state['roles']['panel']['https_port'] = site.https_port if site else None
                json_write(manager.state_file, state)
            except BaseException:
                if ingress_created:
                    Ingress().disable()
                raise
        if handover:
            print(f'安装及迁移完成，已停用选中的旧服务；备份保留于 {handover.backup}。')
            if preview['pending']:
                print(f'{len(preview["pending"])} 项仅保留名称，当前不可用；请在 Web「代理」中手动配置。')
        return True
    except BaseException as original:
        failures = []
        def attempt(label, operation):
            try:
                operation()
            except Exception as error:
                failures.append(f'{label}: {error}')
        if handover and handover.changed:
            # Stop the entire new Agent before returning listening ports to the old services.
            attempt('停止新 Agent', lambda: manager.system.call('stop', 'chain-agent.service'))
            attempt('关闭新 Agent 自启', lambda: manager.system.call('disable', 'chain-agent.service'))
        if checkpoint:
            def restore_panel():
                manager.system.call('stop', 'chain-panel.service')
                with contextlib.closing(sqlite3.connect(checkpoint)) as copy, contextlib.closing(sqlite3.connect(manager.role_paths('panel')[1] / 'panel.db')) as destination:
                    copy.backup(destination)
                manager.system.call('start', 'chain-panel.service')
            attempt('恢复面板数据', restore_panel)
        if acme_undo:
            def restore_acme():
                for path, body in acme_undo.items():
                    if site and not manual and path == site.site:
                        continue  # The HTTPS context already restored its own previous site.
                    if body is None:
                        path.unlink(missing_ok=True)
                    else:
                        atomic_write(path, body, 0o644)
                restore = HttpsSite('rollback.invalid', 0)
                if nginx_before[0]:
                    restore.reload()
                restore.restore_service('nginx', nginx_before)
            attempt('释放新证书验证端口', restore_acme)
        if handover:
            attempt('恢复旧服务', handover.restore)
        if failures:
            raise ValueError(f'安装失败：{original}；恢复未完成：{"；".join(failures)}；备份：{handover.backup if handover else "无旧服务变更"}') from original
        raise
    finally:
        if api:
            with contextlib.suppress(Exception):
                api.call('logout', {})
