"""Install and operate only the independent chain-panel / chain-agent services."""
import argparse
import base64
import binascii
import contextlib
import datetime
import fcntl
import getpass
import grp
import ipaddress
import http.cookiejar
import json
import os
from pathlib import Path
import pwd
import re
import shutil
import shlex
import signal
import socket
import subprocess
import sys
import tarfile
import tempfile
import time
import urllib.parse
import urllib.request
import uuid

from chain_common import (architecture, atomic_write, json_write, output, require, restore_modes,
                          run, safe_extract, sha256, verify_archive_checksum, verify_tree, download_release)

ROOT = Path(__file__).resolve().parents[1]
OWNER = 'chain-panel-installer-v1'
ROLES = ('panel', 'agent')
ROLE_NAMES = {'panel': 'Web 控制台', 'agent': '节点 Agent'}


def decode_connection_code(value):
    try:
        require(value.startswith('chain-agent-v1:') and len(value) < 8192, '接入码格式无效，请从 Web 重新复制。')
        encoded = value.split(':', 1)[1]
        raw = base64.b64decode(encoded + '=' * (-len(encoded) % 4), altchars=b'-_', validate=True)
        document = json.loads(raw)
        require(isinstance(document, dict) and set(document) == {'panel_url', 'node_id', 'token'}
                and all(isinstance(v, str) for v in document.values()), '接入码内容无效。')
        uuid.UUID(document['node_id'])
        require(re.fullmatch(r'[A-Za-z0-9_-]{32,256}', document['token']), '接入码令牌格式无效。')
        validate_panel_url(document['panel_url'])
        return document
    except (ValueError, binascii.Error, UnicodeError, TypeError, AttributeError):
        raise ValueError('接入码无效，请在 Web 的服务器页面重新复制完整接入码。') from None


def validate_panel_url(value):
    u = urllib.parse.urlsplit(value)
    require(u.scheme in ('https', 'http') and u.hostname and not u.username and not u.password
            and not u.query and not u.fragment and u.path in ('', '/')
            and not any(c.isspace() or ord(c) < 32 for c in value), '控制台地址须为完整的 HTTP/HTTPS 根地址。')
    _ = u.port
    return u


def listen_address(value):
    u = urllib.parse.urlsplit('tcp://' + value)
    require(u.hostname and u.port and not u.username and not u.path and not u.query and not u.fragment,
            '监听地址格式：127.0.0.1:8088、0.0.0.0:8088 或 [::1]:8088。')
    ipaddress.ip_address(u.hostname)
    return u.hostname, u.port


def parse_env(path):
    values = {}
    if path.exists():
        for line in path.read_text().splitlines():
            if line and not line.startswith('#'):
                key, value = line.split('=', 1)
                values[key] = json.loads(value)
    return values


def write_env(path, values):
    for key, value in values.items():
        require(re.fullmatch(r'CHAIN_[A-Z_]+', key) and not any(c in value for c in '\n\r\0'), '环境变量包含无效字符。')
    atomic_write(path, ''.join(f'{key}={json.dumps(value, ensure_ascii=False)}\n' for key, value in sorted(values.items())))


class System:
    def __init__(self, enabled):
        self.enabled = enabled

    def call(self, *args, check=True):
        if not self.enabled:
            return 0
        return subprocess.run(['systemctl', *args], check=check).returncode

    def active(self, role):
        return self.enabled and subprocess.run(['systemctl', 'is-active', '--quiet', f'chain-{role}.service'],
                                              stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL).returncode == 0

    def autostart(self, role):
        return self.enabled and subprocess.run(['systemctl', 'is-enabled', '--quiet', f'chain-{role}.service'],
                                              stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL).returncode == 0


class Manager:
    def __init__(self, root=Path('/')):
        self.root = Path(root).resolve()
        self.live = self.root == Path('/')
        if self.live:
            require(os.geteuid() == 0, '安装与运维需要 root 或 sudo。')
            require(Path('/run/systemd/system').is_dir(), '当前安装脚本需要正在运行的 systemd。')
        self.base = self.path('/opt/chain')
        self.state_file = self.base / '.chain-install.json'
        self.system = System(self.live)

    def path(self, absolute):
        return self.root / absolute.lstrip('/')

    def state(self):
        if not self.state_file.exists():
            require(not self.base.exists() or not any(self.base.iterdir()),
                    f'{self.base} 非空且不归本安装器管理，拒绝覆盖。')
            return {'owner': OWNER, 'roles': {}}
        value = json.loads(self.state_file.read_text())
        require(value.get('owner') == OWNER, '安装目录归属不匹配。')
        return value

    @contextlib.contextmanager
    def lock(self):
        self.base.mkdir(parents=True, exist_ok=True)
        if not self.state_file.exists():
            json_write(self.state_file, {'owner': OWNER, 'roles': {}})
        with (self.base / '.install.lock').open('a') as lock:
            try:
                fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            except BlockingIOError:
                raise ValueError('已有安装或运维任务运行，请稍后重试。') from None
            yield

    def role_paths(self, role):
        require(role in ROLES, '组件只能是 panel 或 agent。')
        return (self.path('/etc/chain-' + role), self.path('/var/lib/chain-' + role),
                self.path('/etc/systemd/system/chain-' + role + '.service'))

    def require_role(self, role):
        state = self.state()
        require(role in state['roles'], f'尚未安装 {role}，请先执行 install。')
        return state

    def account(self, role):
        if not self.live:
            return os.geteuid(), os.getegid()
        name, home = 'chain-' + role, '/var/lib/chain-' + role
        try:
            user = pwd.getpwnam(name)
            require(user.pw_uid != 0 and user.pw_dir == home and user.pw_shell.endswith(('nologin', 'false')),
                    f'已有用户 {name} 的属性与服务用户不一致。')
        except KeyError:
            try:
                grp.getgrnam(name)
            except KeyError:
                pass
            else:
                raise ValueError(f'已有组 {name}，无法确认归属。')
            run(['useradd', '--system', '--user-group', '--no-create-home', '--home-dir', home,
                 '--shell', '/usr/sbin/nologin', name])
            user = pwd.getpwnam(name)
        return user.pw_uid, user.pw_gid

    def config(self, role, args):
        etc, data, _ = self.role_paths(role)
        env = parse_env(etc / (role + '.env'))
        env['CHAIN_XRAY_BIN'] = str(self.base / ('current-' + role) / 'bin/xray')
        token = None
        if role == 'panel':
            env['CHAIN_DATA_DIR'] = str(data)
            env['CHAIN_RELEASE_DIR'] = str(self.base / 'current-panel')
            env['CHAIN_LISTEN'] = args.listen or env.get('CHAIN_LISTEN', '127.0.0.1:8088')
            env['CHAIN_SECURE_COOKIE'] = args.secure_cookie if args.secure_cookie is not None else env.get('CHAIN_SECURE_COOKIE', '0')
            listen_address(env['CHAIN_LISTEN'])
            if getattr(args, 'public_url', None) is not None:
                env.pop('CHAIN_PUBLIC_URL', None)
                if args.public_url:
                    validate_panel_url(args.public_url)
                    env['CHAIN_PUBLIC_URL'] = args.public_url.rstrip('/')
        else:
            env['CHAIN_AGENT_DIR'] = str(data)
            env['CHAIN_NODE_TOKEN_FILE'] = str(etc / 'token')
            url = args.panel_url or env.get('CHAIN_PANEL_URL')
            node = args.node_id or env.get('CHAIN_NODE_ID')
            if not url and sys.stdin.isatty():
                url = input('控制台 HTTPS URL: ').strip()
            if not node and sys.stdin.isatty():
                node = input('节点 UUID（在 Web 添加服务器后获取）: ').strip()
            require(url and node, 'Agent 需要 --panel-url 和 --node-id。')
            u = urllib.parse.urlsplit(url)
            require(u.hostname and not u.username and not u.password and not u.query and not u.fragment
                    and u.path in ('', '/') and not any(c.isspace() for c in url), '控制台 URL 无效，应为根地址且不含凭据。')
            loopback = u.hostname == 'localhost'
            with contextlib.suppress(ValueError):
                loopback = ipaddress.ip_address(u.hostname).is_loopback
            allow_http = args.allow_http or env.get('CHAIN_ALLOW_HTTP') == '1'
            require(u.scheme == 'https' or (u.scheme == 'http' and (loopback or allow_http)),
                    '非本机控制台需要 HTTPS；可信私网可显式加 --allow-http。')
            uuid.UUID(node)
            env.update(CHAIN_PANEL_URL=url.rstrip('/'), CHAIN_NODE_ID=node)
            if allow_http:
                env['CHAIN_ALLOW_HTTP'] = '1'
            if getattr(args, 'node_token', None):
                token = args.node_token
            elif args.token_file:
                path = Path(args.token_file).resolve(strict=True)
                require(path.stat().st_mode & 0o077 == 0, '令牌文件权限必须为 600，请先 chmod 600。')
                token = path.read_text().strip()
            elif not (etc / 'token').is_file() and sys.stdin.isatty():
                token = getpass.getpass('节点令牌（不回显）: ').strip()
            require(token is not None or (etc / 'token').is_file(), '请通过 --token-file 指定节点令牌文件。')
            if token is not None:
                require(re.fullmatch(r'[A-Za-z0-9_-]{32,256}', token), '节点令牌格式无效。')
        return env, token

    def unit(self, role):
        etc, _, _ = self.role_paths(role)
        env_file = str(etc / (role + '.env')).replace('%', '%%')
        require(not any(c in env_file for c in '\n\r\0'), '服务路径包含无效字符。')
        def quote(value):
            return '"' + str(value).replace('\\', '\\\\').replace('"', '\\"').replace('%', '%%') + '"'
        capability = 'AmbientCapabilities=CAP_NET_BIND_SERVICE\nCapabilityBoundingSet=CAP_NET_BIND_SERVICE\n' if role == 'agent' else ''
        return (f'# {OWNER}\n[Unit]\nDescription=Chain {role}\nAfter=network-online.target\nWants=network-online.target\n\n'
                f'[Service]\nType=simple\nUser=chain-{role}\nGroup=chain-{role}\nStateDirectory=chain-{role}\n'
                f'StateDirectoryMode=0700\nUMask=0077\nEnvironmentFile={env_file}\n'
                f'ExecStart={quote(self.base / ("current-" + role) / "bin" / ("chain-" + role))}\n'
                'Restart=on-failure\nRestartSec=5\nKillMode=control-group\nTimeoutStopSec=50\n'
                'NoNewPrivileges=true\nPrivateTmp=true\nProtectSystem=strict\nProtectHome=true\n' + capability +
                '\n[Install]\nWantedBy=multi-user.target\n')

    def link(self, role, release):
        link = self.base / ('current-' + role)
        require(re.fullmatch(r'[A-Za-z0-9_.-]+', release), '发行目录名称无效。')
        temp = self.base / ('.link-' + uuid.uuid4().hex)
        try:
            temp.symlink_to('releases/' + release)
            os.replace(temp, link)
        finally:
            temp.unlink(missing_ok=True)

    def healthy(self, role, env):
        if not self.live:
            return
        host, port = listen_address(env['CHAIN_LISTEN']) if role == 'panel' else ('', 0)
        host = {'0.0.0.0': '127.0.0.1', '::': '::1'}.get(host, host)
        if ':' in host:
            host = '[' + host + ']'
        opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
        for _ in range(20):
            if self.system.active(role):
                if role == 'agent':
                    time.sleep(1)
                    if self.system.active(role):
                        return
                else:
                    try:
                        with opener.open(f'http://{host}:{port}/', timeout=1) as response:
                            if response.status == 200 and b'<!doctype html>' in response.read(100).lower():
                                return
                    except OSError:
                        pass
            time.sleep(.5)
        raise ValueError(f'chain-{role} 启动检查未通过；请查看 journalctl -u chain-{role}。')

    def backup_files(self, role):
        etc, data, _ = self.role_paths(role)
        directory = self.path('/var/backups/chain-panel')
        directory.mkdir(parents=True, exist_ok=True, mode=0o700)
        name = f'{role}-{datetime.datetime.now(datetime.timezone.utc):%Y%m%dT%H%M%SZ}-{uuid.uuid4().hex[:8]}.tar.gz'
        destination = directory / name
        with destination.open('xb') as stream:
            os.fchmod(stream.fileno(), 0o600)
            with tarfile.open(fileobj=stream, mode='w:gz') as bundle:
                for prefix, source in [('config', etc), ('data', data)]:
                    if source.exists():
                        for path in sorted(source.rglob('*')):
                            require(not path.is_symlink(), '备份路径包含符号链接，拒绝继续。')
                            if path.is_file() and path.name not in ('xray.log', 'process.lock', 'candidate.json'):
                                bundle.add(path, arcname=prefix + '/' + str(path.relative_to(source)), recursive=False)
        print(f'私有备份：{destination}（权限 600，含连接凭据，请私下保存）')
        return destination

    def install(self, source, manifest, args):
        role = args.component
        state = self.state()
        old = state['roles'].get(role)
        require(args.action != 'update' or old, f'尚未安装 {role}；首次请用 install。')
        require(manifest.get('arch') == architecture(), '发行包与本机 CPU 架构不匹配。')
        current_glibc = tuple(map(int, output(['getconf', 'GNU_LIBC_VERSION']).split()[-1].split('.')))
        require(current_glibc >= tuple(map(int, manifest['glibc_min'].split('.'))),
                f'发行包要求 glibc ≥ {manifest["glibc_min"]}；请在更旧系统构建兼容包或在目标机原生构建。')
        etc, data, unit = self.role_paths(role)
        wrapper = self.path('/usr/local/bin/chainctl')
        if not state['roles']:
            require(not wrapper.exists(), f'{wrapper} 已存在且不归本安装器管理。')
        if old is None:
            require(not unit.exists(), f'{unit} 已存在，拒绝覆盖其他安装方式创建的服务。')
            for directory in (etc, data):
                require(not directory.exists() or not any(directory.iterdir()), f'{directory} 非空，拒绝接管。')
        else:
            require(unit.is_file() and OWNER in unit.read_text(), '服务文件归属不匹配。')
        env, token = self.config(role, args)
        if self.live and role == 'panel' and any(not shutil.which(name) for name in ('ssh', 'ssh-keyscan', 'tar')):
            print('正在安装 Web 远程接入所需的 SSH 客户端…')
            if shutil.which('apt-get'):
                run(['apt-get', 'update'])
                run(['apt-get', 'install', '-y', 'openssh-client', 'tar', 'gzip'])
            elif shutil.which('dnf'):
                run(['dnf', 'install', '-y', 'openssh-clients', 'tar', 'gzip'])
            else:
                raise ValueError('Web 远程安装需要 openssh-client、tar 和 gzip，请先安装这些系统软件。')
        old_env = parse_env(etc / (role + '.env'))
        if self.live and role == 'panel' and (not self.system.active(role) or old_env.get('CHAIN_LISTEN') != env['CHAIN_LISTEN']):
            host, port = listen_address(env['CHAIN_LISTEN'])
            with socket.socket(socket.AF_INET6 if ':' in host else socket.AF_INET, socket.SOCK_STREAM) as check:
                check.bind((host, port))  # Never stop an existing port owner.
        uid, gid = self.account(role)
        release_id = f'{manifest["version"]}-linux-{manifest["arch"]}-{sha256(source / "release.json")[:12]}'
        release = self.base / 'releases' / release_id
        with self.lock():
            state = self.state()
            require(state['roles'].get(role) == old, '安装状态已变化，请重试。')
            if release.exists():
                require(verify_tree(release) == manifest, '已存在的发行目录与安装包不一致。')
            else:
                release.parent.mkdir(parents=True, exist_ok=True)
                temporary = release.parent / ('.install-' + uuid.uuid4().hex)
                try:
                    shutil.copytree(source, temporary)
                    restore_modes(temporary, manifest)
                    os.rename(temporary, release)
                finally:
                    if temporary.exists():
                        shutil.rmtree(temporary)
            configs = [etc / (role + '.env'), etc / 'token', unit]
            previous = {p: p.read_bytes() if p.exists() else None for p in configs}
            was_active, was_enabled = self.system.active(role), self.system.autostart(role)
            try:
                if old:
                    self.system.call('stop', 'chain-' + role + '.service')
                    self.backup_files(role)
                for directory in (etc, data):
                    directory.mkdir(parents=True, exist_ok=True, mode=0o700)
                    directory.chmod(0o700)
                    os.chown(directory, uid, gid)
                write_env(etc / (role + '.env'), env)
                if token is not None:
                    atomic_write(etc / 'token', token + '\n')
                for path in configs[:2]:
                    if path.exists():
                        os.chown(path, uid, gid)
                atomic_write(unit, self.unit(role), 0o644)
                self.link(role, release_id)
                self.system.call('daemon-reload')
                self.system.call('enable', 'chain-' + role + '.service')
                self.system.call('restart', 'chain-' + role + '.service')
                self.healthy(role, env)
                state['roles'][role] = {'current': release_id, 'previous': old['current'] if old and old['current'] != release_id else (old or {}).get('previous'),
                                        'version': manifest['version']}
                json_write(self.state_file, state)
                manager = self.base / 'manager'
                manager.mkdir(exist_ok=True)
                for name in ['chain_install.py', 'chain_common.py']:
                    atomic_write(manager / name, (release / 'scripts' / name).read_bytes(), 0o644)
                atomic_write(wrapper, f'#!/usr/bin/env bash\n# {OWNER}\nset -Eeuo pipefail\nexec python3 -B {shlex.quote(str(manager / "chain_install.py"))} "$@"\n', 0o755)
            except BaseException:
                self.system.call('stop', 'chain-' + role + '.service', check=False)
                for path, value in previous.items():
                    if value is None:
                        path.unlink(missing_ok=True)
                    else:
                        atomic_write(path, value, 0o644 if path == unit else 0o600)
                        if path != unit:
                            os.chown(path, uid, gid)
                if old:
                    self.link(role, old['current'])
                    state['roles'][role] = old
                else:
                    (self.base / ('current-' + role)).unlink(missing_ok=True)
                    state['roles'].pop(role, None)
                json_write(self.state_file, state)
                self.system.call('daemon-reload', check=False)
                if not was_enabled:
                    self.system.call('disable', 'chain-' + role + '.service', check=False)
                if was_active:
                    self.system.call('start', 'chain-' + role + '.service')
                print('安装未完成，已恢复此前程序链接与配置；数据与备份保留。', file=sys.stderr)
                raise
        if self.live:
            print(f'chain-{role} 已安装并启动；日常运维：chainctl menu')
            if role == 'panel':
                self.show_access()
            else:
                print('Agent 已启动，请回到 Web「服务器」页面等待显示在线。排障时输入 chainctl，选择「查看日志」。')
        else:
            print(f'离线安装树已准备：{self.root}；未创建系统账户、未操作 systemd、未启动监听服务。')

    def show_access(self):
        self.require_role('panel')
        etc, data, _ = self.role_paths('panel')
        env = parse_env(etc / 'panel.env')
        host, port = listen_address(env['CHAIN_LISTEN'])
        url = env.get('CHAIN_PUBLIC_URL')
        if url:
            print(f'浏览器打开：{url}')
        elif host in ('127.0.0.1', '::1'):
            print(f'在自己电脑建立 SSH 隧道：ssh -N -L 18088:127.0.0.1:{port} root@服务器IP\n浏览器打开：http://127.0.0.1:18088')
        else:
            print(f'浏览器打开：http://服务器IP:{port}')
        token = data / 'admin-token'
        if token.is_file() and sys.stdin.isatty():
            print(f'登录管理令牌：{token.read_text().strip()}')
        else:
            print('输入 chainctl，在菜单选择「查看登录信息」。')
        print('日常管理只需输入 chainctl；命令帮助：chainctl help。')

    def rollback(self, role):
        self.require_role(role)
        with self.lock():
            state = self.require_role(role)
            current = state['roles'][role]
            prior = current.get('previous')
            require(prior, '没有可回滚的旧版本。')
            manifest = verify_tree(self.base / 'releases' / prior)
            etc, _, _ = self.role_paths(role)
            env = parse_env(etc / (role + '.env'))
            active = self.system.active(role)
            self.system.call('stop', 'chain-' + role + '.service')
            try:
                self.backup_files(role)
                self.link(role, prior)
                self.system.call('start', 'chain-' + role + '.service')
                self.healthy(role, env)
                state['roles'][role] = {'current': prior, 'previous': current['current'], 'version': manifest['version']}
                json_write(self.state_file, state)
            except BaseException:
                self.system.call('stop', 'chain-' + role + '.service', check=False)
                self.link(role, current['current'])
                if active:
                    self.system.call('start', 'chain-' + role + '.service')
                raise
        print('程序版本已回滚，业务数据与配置保留；没有将数据库恢复到旧备份。')

    def operate(self, args):
        role, action = args.component, args.action
        state = self.require_role(role)
        if action == 'status':
            print(f'{ROLE_NAMES[role]} · 版本 {state["roles"][role]["version"]}')
            self.system.call('status', 'chain-' + role + '.service', '--no-pager', check=False)
        elif action == 'logs':
            require(self.live, '--root 离线安装树没有 systemd 日志。')
            run(['journalctl', '-u', 'chain-' + role + '.service', '--no-pager', '-n', '100'])
        elif action == 'token':
            require(role == 'panel', 'Agent 令牌只在 Web 创建或重置时显示。')
            print((self.role_paths(role)[1] / 'admin-token').read_text().strip())
        elif action == 'access':
            self.show_access()
        elif action == 'rollback':
            self.rollback(role)
        else:
            with self.lock():
                if action == 'backup':
                    active = self.system.active(role)
                    self.system.call('stop', 'chain-' + role + '.service')
                    try:
                        self.backup_files(role)
                    finally:
                        if active:
                            self.system.call('start', 'chain-' + role + '.service')
                else:
                    self.system.call(action, 'chain-' + role + '.service')
                    if action in ('start', 'restart'):
                        etc, _, _ = self.role_paths(role)
                        self.healthy(role, parse_env(etc / (role + '.env')))


def local_packages():
    return sorted((ROOT / 'release-output').glob(f'*/chain-panel-*-linux-{architecture()}.tar.gz'),
                  key=lambda p: p.stat().st_mtime)


def provision_local_agent(manager, source, manifest, args):
    if 'agent' in manager.state()['roles']:
        return
    etc, data, _ = manager.role_paths('panel')
    env = parse_env(etc / 'panel.env')
    host, port = listen_address(env['CHAIN_LISTEN'])
    host = {'0.0.0.0': '127.0.0.1', '::': '::1'}.get(host, host)
    base = f'http://[{host}]:{port}' if ':' in host else f'http://{host}:{port}'
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
    cookie = ''
    def api(path, payload, method='POST'):
        nonlocal cookie
        request = urllib.request.Request(base + '/api/' + path, data=json.dumps(payload).encode(), method=method,
                                         headers={'Content-Type': 'application/json', 'X-Chain-Request': '1', 'Cookie': cookie})
        with opener.open(request, timeout=15) as response:
            if path == 'login':
                cookie = response.headers['Set-Cookie'].split(';', 1)[0]
            return json.loads(response.read())
    node = None
    try:
        api('login', {'token': (data / 'admin-token').read_text().strip()})
        public_host = getattr(args, 'local_host', None) or urllib.parse.urlsplit(env.get('CHAIN_PUBLIC_URL', '')).hostname or '127.0.0.1'
        target = getattr(args, 'local_target', None) or 'www.microsoft.com:443'
        name = urllib.parse.urlsplit('tcp://' + target).hostname
        node = api('nodes', {'name': '本机节点', 'host': public_host, 'target': target, 'server_name': name})
        agent = make_parser().parse_args(['install', '--component', 'agent', '--root', str(manager.root), '--non-interactive'])
        agent.panel_url, agent.node_id, agent.node_token = base, node['id'], node['token']
        manager.install(source, manifest, agent)
        print('本机 Agent 已安装并自动接入；在 Web「服务器」中查看本机节点即可。')
    except BaseException:
        if node is not None and 'agent' not in manager.state()['roles']:
            with contextlib.suppress(Exception):
                api('nodes/' + node['id'], {}, 'DELETE')
        print('控制台已保留；本机 Agent 接入未完成，可再次运行安装向导重试。', file=sys.stderr)
        raise
    finally:
        if cookie:
            with contextlib.suppress(Exception):
                api('logout', {})


def install_components(manager, source, manifest, args):
    manager.install(source, manifest, args)
    if manager.live and args.component == 'panel' and args.action == 'install' and not getattr(args, 'panel_only', False):
        provision_local_agent(manager, source, manifest, args)


def install_package(manager, args):
    if getattr(args, 'reuse_current', False):
        source = (manager.base / ('current-' + args.component)).resolve(strict=True)
        install_components(manager, source, verify_tree(source), args)
        return
    latest = getattr(args, 'latest', False) or (args.action == 'update' and not args.package)
    if not args.package and not latest and (ROOT / 'release.json').is_file():
        install_components(manager, ROOT, verify_tree(ROOT), args)
        return
    choices = local_packages() if not args.package and not latest else []
    archive = Path(args.package).resolve(strict=True) if args.package else choices[-1] if choices else None
    parent = Path(args.work_parent).resolve(strict=True) if args.work_parent else manager.root / 'var/tmp'
    parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix='chain-install-', dir=parent) as temporary:
        expected_version = None
        if archive is None:
            archive, expected_version = download_release(Path(temporary))
        verify_archive_checksum(archive)
        stage = Path(temporary) / 'stage'
        manifest = safe_extract(archive, stage)
        require(expected_version is None or manifest['version'] == expected_version, '下载包版本与发行信息不一致。')
        print(f'正在安装 {ROLE_NAMES[args.component]} v{manifest["version"]}…', flush=True)
        install_components(manager, stage, manifest, args)


def choose(title, options, default='1'):
    while True:
        print('\n' + title)
        for key, label in options.items():
            print(f'{key}) {label}')
        value = input(f'选择 [{default}]: ').strip() or default
        if value in options:
            return value
        print('请输入上面的选项编号。')


def panel_questions(manager, args):
    etc, _, _ = manager.role_paths('panel')
    prior = parse_env(etc / 'panel.env')
    if args.listen:
        return
    options = {'1': '浏览器直连（HTTP，适用于可信网络）', '2': '已有 HTTPS 反向代理', '3': '仅本机，通过 SSH 隧道访问'}
    if prior:
        options = {'0': f'保留当前访问方式（{prior["CHAIN_LISTEN"]}）', **options}
    mode = choose('控制台访问方式', options, '0' if prior else '1')
    if mode == '0':
        return
    while True:
        value = input('控制台端口 [8088]: ').strip() or '8088'
        if value.isdigit() and 0 < int(value) <= 65535:
            break
        print('请输入 1–65535 之间的端口。')
    args.listen = ('0.0.0.0:' if mode == '1' else '127.0.0.1:') + value
    args.secure_cookie = '1' if mode == '2' else '0'
    args.public_url = ''
    if mode == '1':
        while True:
            host = input('这台服务器可访问的 IP 或域名: ').strip()
            try:
                u = validate_panel_url(f'http://{host}:{value}')
                require(u.hostname == host and ':' not in host, '请填写 IPv4 地址或域名。')
                args.public_url = f'http://{host}:{value}'
                break
            except ValueError:
                print('请输入服务器 IPv4 地址或域名，不要包含协议或路径。')
    elif mode == '2':
        while True:
            url = input('已有 HTTPS 控制台地址（如 https://panel.example.com）: ').strip()
            try:
                require(validate_panel_url(url).scheme == 'https', '需要 HTTPS 地址。')
                args.public_url = url.rstrip('/')
                break
            except ValueError as error:
                print(error)
        print(f'请将该域名代理到 127.0.0.1:{value}，并保留 Host；向导不会自动安装反向代理。')


def agent_questions(manager, args):
    if args.panel_url and args.node_id and args.token_file:
        return
    installed = 'agent' in manager.state()['roles']
    print('在 Web「服务器」中添加节点或重置接入码，然后复制完整接入码。')
    while True:
        value = getpass.getpass('粘贴接入码（不回显）' + ('，直接回车保留当前身份' if installed else '') + ': ').strip()
        if not value and installed:
            return
        try:
            document = decode_connection_code(value)
            break
        except ValueError as error:
            print(error)
    args.panel_url, args.node_id, args.node_token = document['panel_url'], document['node_id'], document['token']
    url = validate_panel_url(args.panel_url)
    loopback = url.hostname == 'localhost'
    with contextlib.suppress(ValueError):
        loopback = ipaddress.ip_address(url.hostname).is_loopback
    if url.scheme == 'http' and not loopback and not args.allow_http:
        require(choose('此控制台使用 HTTP；仅在可信私网使用该连接。',
                       {'1': '这是可信私网，继续', '2': '取消，改用 HTTPS 地址'}, '2') == '1', '已取消接入。')
        args.allow_http = True
    print(f'接入控制台：{args.panel_url}；凭据由安装器保存。')


def install_wizard(manager, args):
    print(f'\n{ROLE_NAMES[args.component]} · 安装向导')
    if args.action == 'install':
        (panel_questions if args.component == 'panel' else agent_questions)(manager, args)
        if args.component == 'panel' and not args.panel_only and 'agent' not in manager.state()['roles']:
            args.panel_only = choose('是否让控制台这台机器也参与转发？',
                                    {'1': '一起安装本机 Agent 并自动接入（推荐）', '2': '仅安装控制台，转发交给其他机器'}) == '2'
            if not args.panel_only:
                if not args.local_host:
                    prior = parse_env(manager.role_paths('panel')[0] / 'panel.env')
                    default = urllib.parse.urlsplit(args.public_url or prior.get('CHAIN_PUBLIC_URL', '')).hostname or ''
                    args.local_host = input(f'其他节点访问本机的 IP / 域名 [{default or "必填"}]: ').strip() or default
                    require(args.local_host, '请填写本机可访问地址。')
                if not args.local_target:
                    args.local_target = input('本机 REALITY TLS 目标 [www.microsoft.com:443]: ').strip() or 'www.microsoft.com:443'
    if not args.package and not args.latest:
        if args.action == 'install' and args.component in manager.state()['roles']:
            args.reuse_current = True
        else:
            source = choose('安装来源', {'1': '自动选择（本地包优先，否则下载正式版本）',
                                       '2': '下载最新正式版本', '3': '选择本地安装包'},
                            '2' if args.action == 'update' else '1')
            args.latest = source == '2'
            if source == '3':
                args.package = Path(input('安装包文件路径: ').strip()).expanduser()
    install_package(manager, args)


def select_role(manager):
    installed = list(manager.state()['roles'])
    require(installed, '请先从菜单安装控制台或接入节点。')
    if len(installed) == 1:
        return installed[0]
    options = {str(i + 1): ROLE_NAMES[role] for i, role in enumerate(installed)}
    return installed[int(choose('选择要管理的组件', options)) - 1]


def menu(root):
    manager = Manager(root)
    while True:
        installed = manager.state()['roles']
        print('\nChain Panel · 管理菜单')
        for role, value in installed.items():
            running = ('运行中' if manager.system.active(role) else '已停止') if manager.live else '离线安装树'
            print(f'  {ROLE_NAMES[role]}  v{value["version"]}  {running}')
        choice = choose('请选择操作', {
            '1': '安装 / 配置控制台与本机节点', '2': '接入 / 重新接入节点', '3': '更新到最新版本',
            '4': '查看状态', '5': '查看日志', '6': '重启服务', '7': '备份数据',
            '8': '回滚上一版本', '9': '查看登录信息', '10': '启动服务', '11': '停止服务',
            '12': '查看命令帮助', '0': '退出'}, '4' if installed else '1')
        if choice == '0':
            return
        try:
            if choice == '12':
                make_parser().print_help()
                continue
            role = 'panel' if choice in ('1', '9') else 'agent' if choice == '2' else select_role(manager)
            action = {'1': 'install', '2': 'install', '3': 'update', '4': 'status', '5': 'logs',
                      '6': 'restart', '7': 'backup', '8': 'rollback', '9': 'access', '10': 'start', '11': 'stop'}[choice]
            args = make_parser().parse_args([action, '--component', role, '--root', str(root)])
            if action in ('install', 'update'):
                install_wizard(manager, args)
            else:
                manager.operate(args)
        except (ValueError, OSError, subprocess.CalledProcessError) as error:
            print(f'操作未完成：{error}', file=sys.stderr)


def make_parser():
    parser = argparse.ArgumentParser(prog='chainctl', formatter_class=argparse.RawDescriptionHelpFormatter,
        description='直接输入 chainctl 打开管理菜单。安装、接入、升级和备份都可以按提示完成。',
        epilog="""常用入口：
  chainctl                  打开交互管理菜单
  chainctl help             查看本帮助（无需管理员权限）
  chainctl install          引导安装 Web 控制台
  chainctl install --component agent   粘贴 Web 接入码接入节点
  chainctl update           自动下载最新正式版本并升级

高级命令（也可在菜单中选择，无需记忆）：
  status / logs             查看状态 / 最近日志
  start / stop / restart    启动 / 停止 / 重启
  backup / rollback         私有备份 / 回滚程序版本
  access / token            查看登录信息 / 仅输出管理令牌
  --component agent         操作节点 Agent，默认操作控制台
  --package 文件.tar.gz      使用本地发行包，旁边需有 SHA256SUMS
  --non-interactive         自动化调用，不打开安装向导

安装脚本同样支持 help 和以上子命令。""")
    parser.add_argument('action', nargs='?', default='menu', choices=['menu', 'help', 'install', 'update', 'status', 'start', 'stop', 'restart', 'logs', 'backup', 'rollback', 'access', 'token'])
    parser.add_argument('--component', choices=ROLES, default='panel', help='默认 panel；节点机器选择 agent')
    parser.add_argument('--package', type=Path, help='本地发行包，同目录需有 SHA256SUMS')
    parser.add_argument('--latest', action='store_true', help='下载最新正式版本')
    parser.add_argument('--non-interactive', action='store_true', help='不进行交互提问')
    parser.add_argument('--listen', help='控制台监听地址；交互安装会引导选择')
    parser.add_argument('--secure-cookie', choices=['0', '1'], help='HTTPS 反向代理设为 1')
    parser.add_argument('--public-url', help='浏览器访问地址；不会自动配置反向代理')
    parser.add_argument('--panel-only', action='store_true', help='仅安装控制台，跳过默认的本机 Agent 自动接入')
    parser.add_argument('--local-host', help='本机节点对其他服务器可达的地址')
    parser.add_argument('--local-target', help='本机 REALITY TLS 目标，默认 www.microsoft.com:443')
    parser.add_argument('--panel-url', help='Agent 连接地址；推荐在向导粘贴 Web 接入码')
    parser.add_argument('--node-id', help='高级：Agent 节点 UUID')
    parser.add_argument('--token-file', type=Path, help='高级：权限 600 的节点令牌文件')
    parser.add_argument('--allow-http', action='store_true', help='明确允许可信私网的 HTTP 控制台')
    parser.add_argument('--work-parent', type=Path, help='安装临时目录的父目录')
    parser.add_argument('--root', type=Path, default=Path('/'), help='测试用离线安装树，不操作系统服务')
    return parser


def main(argv=None):
    argv = list(sys.argv[1:] if argv is None else argv)
    parser = make_parser()
    args = parser.parse_args(argv)
    if args.action == 'help' or (args.action == 'menu' and not sys.stdin.isatty()):
        parser.print_help()
        return
    if args.root.resolve() == Path('/') and os.geteuid() != 0:
        require(shutil.which('sudo'), '安装与运维需要管理员权限，请使用 root 运行。')
        run(['sudo', sys.executable, '-B', Path(__file__).resolve(), *argv])
        return
    if args.action == 'menu':
        return menu(args.root)
    manager = Manager(args.root)
    if args.action in ('install', 'update'):
        if sys.stdin.isatty() and not args.non_interactive:
            install_wizard(manager, args)
        else:
            install_package(manager, args)
    else:
        manager.operate(args)


if __name__ == '__main__':
    signal.signal(signal.SIGTERM, lambda *_: sys.exit(143))
    try:
        main()
    except (ValueError, OSError, subprocess.CalledProcessError, KeyboardInterrupt, EOFError) as error:
        raise SystemExit(str(error) or '操作已中断。')
