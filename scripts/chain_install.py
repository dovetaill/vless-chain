"""Install and operate only the independent chain-panel / chain-agent services."""
import argparse
import base64
import binascii
import contextlib
import datetime
import fcntl
import getpass
import grp
import hashlib
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
import sqlite3
import subprocess
import sys
import tarfile
import tempfile
import time
import urllib.parse
import urllib.request
import uuid

from chain_common import (architecture, operating_system, compatible_release, atomic_write, json_write, output, require, restore_modes,
                          run, safe_extract, sha256, verify_archive_checksum, verify_tree, download_release)
from chain_platform import (service_kind, service_path, service, service_state, service_definition,
                            create_account, remove_account, nginx_conf, init_path)
from chain_network import (HttpsSite, SITE_OWNER, available_port, domain_name, public_ipv4, server_addresses, validate_dns)

ROOT = Path(__file__).resolve().parents[1]
OWNER = 'chain-panel-installer-v1'
ROLES = ('panel', 'agent')
ROLE_NAMES = {'panel': 'Web 控制台', 'agent': '节点 Agent'}

# Use terminal line editing so arrow keys do not appear as escape sequences.
readline = None
with contextlib.suppress(ImportError):
    import readline
    readline.set_auto_history(False)


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
        if service_kind() != 'systemd':
            return 0 if args[0] == 'daemon-reload' else service(args[0], args[1], check=check)
        return subprocess.run(['systemctl', *args], check=check).returncode

    def active(self, role):
        if service_kind() != 'systemd':
            return self.enabled and service_state('chain-' + role)[0]
        return self.enabled and subprocess.run(['systemctl', 'is-active', '--quiet', f'chain-{role}.service'],
                                              stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL).returncode == 0

    def autostart(self, role):
        if service_kind() != 'systemd':
            return self.enabled and service_state('chain-' + role)[1]
        return self.enabled and subprocess.run(['systemctl', 'is-enabled', '--quiet', f'chain-{role}.service'],
                                              stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL).returncode == 0


class Manager:
    def __init__(self, root=Path('/')):
        self.root = Path(root).resolve()
        self.live = self.root == Path('/')
        if self.live:
            require(os.geteuid() == 0, '安装与运维需要 root 或 sudo。')
            require(service_kind() != 'systemd' or Path('/run/systemd/system').is_dir(),
                    'Linux 安装需要正在运行的 systemd 或 OpenRC。')
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

    def panel_setup_pending(self):
        panel = self.state()['roles'].get('panel')
        if panel is None:
            return False
        if 'setup_complete' in panel:
            return not panel['setup_complete']
        env = parse_env(self.role_paths('panel')[0] / 'panel.env')
        # Older installers committed the roles before configuring HTTPS.
        return (self.live and env.get('CHAIN_PUBLIC_URL', '').startswith('https://')
                and not shutil.which('nginx') and not panel.get('https_port'))

    def complete_panel_setup(self):
        with self.lock():
            state = self.state()
            state['roles']['panel']['setup_complete'] = True
            json_write(self.state_file, state)

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
                self.path(str(service_path('chain-' + role))))

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
            create_account(name, home)
            user = pwd.getpwnam(name)
        return user.pw_uid, user.pw_gid

    def config(self, role, args):
        etc, data, _ = self.role_paths(role)
        env = parse_env(etc / (role + '.env'))
        env['CHAIN_XRAY_BIN'] = str(self.base / ('current-' + role) / 'bin/xray')
        env['CHAIN_ACME_WEBROOT'] = str(self.path('/var/lib/chain-acme') / role)
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
        if service_kind() != 'systemd':
            return service_definition('chain-' + role, [self.base / ('current-' + role) / 'bin' / ('chain-' + role)],
                                      OWNER, 'chain-' + role, parse_env(etc / (role + '.env')))
        env_file = str(etc / (role + '.env')).replace('%', '%%')
        require(not any(c in env_file for c in '\n\r\0'), '服务路径包含无效字符。')
        def quote(value):
            return '"' + str(value).replace('\\', '\\\\').replace('"', '\\"').replace('%', '%%') + '"'
        capability = 'AmbientCapabilities=CAP_NET_BIND_SERVICE\nCapabilityBoundingSet=CAP_NET_BIND_SERVICE\n' if role == 'agent' else ''
        return (f'# {OWNER}\n[Unit]\nDescription=Chain {role}\nAfter=network-online.target\nWants=network-online.target\n\n'
                f'[Service]\nType=simple\nUser=chain-{role}\nGroup=chain-{role}\nStateDirectory=chain-{role}\n'
                f'StateDirectoryMode=0700\nUMask=0077\nEnvironmentFile={env_file}\n'
                f'ExecStart={quote(self.base / ("current-" + role) / "bin" / ("chain-" + role))}\n'
                'Restart=on-failure\nRestartSec=5\nKillMode=control-group\nTimeoutStopSec=50\n' +
                ('LimitNOFILE=65536\n' if role == 'agent' else '') +
                'NoNewPrivileges=true\nPrivateTmp=true\nProtectSystem=strict\nProtectHome=true\n' + capability +
                f'ReadWritePaths={quote(self.path("/var/lib/chain-acme") / role)}\n' +
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

    def maintenance_units(self):
        if service_kind() != 'systemd':
            return
        script = self.base / 'current-panel/scripts/chain_maintenance.py'
        if not script.is_file():
            return
        definitions = {
            'socket': (f'# {OWNER}\n[Unit]\nDescription=Chain maintenance socket\n'
                       '[Socket]\nListenStream=/run/chain-maintenance/control.sock\n'
                       'SocketUser=root\nSocketGroup=chain-panel\nSocketMode=0660\nDirectoryMode=0755\nRemoveOnStop=true\n'
                       '[Install]\nWantedBy=sockets.target\n'),
            'service': (f'# {OWNER}\n[Unit]\nDescription=Chain restricted maintenance executor\n'
                        '[Service]\nUser=root\nGroup=root\nUMask=0077\nStateDirectory=chain-maintenance\n'
                        'StateDirectoryMode=0700\nEnvironment=PYTHONDONTWRITEBYTECODE=1\n'
                        f'ExecStart="{sys.executable}" -B "{script}"\n'
                        'PrivateTmp=true\nProtectHome=true\nKillMode=control-group\nTimeoutStopSec=45\n'),
        }
        for suffix in definitions:
            path = self.path('/etc/systemd/system/chain-maintenance.' + suffix)
            require(not path.is_symlink() and (not path.exists() or OWNER in path.read_text()),
                    f'{path} 不归本安装器管理，拒绝覆盖。')
        runtime = self.path('/run/chain-maintenance')
        if runtime.exists() or runtime.is_symlink():
            socket_unit = self.path('/etc/systemd/system/chain-maintenance.socket')
            require(runtime.is_dir() and not runtime.is_symlink() and socket_unit.is_file()
                    and OWNER in socket_unit.read_text() and (not self.live or runtime.stat().st_uid == 0),
                    '维护运行目录归属不匹配，拒绝接管。')
        state = self.path('/var/lib/chain-maintenance')
        marker = state / '.chain-owner'
        require(not state.is_symlink() and not os.path.ismount(state), '维护目录不能是链接或挂载点。')
        if state.exists():
            require(marker.is_file() and not marker.is_symlink() and marker.read_text() == OWNER,
                    '维护目录归属不匹配，拒绝接管。')
            require(state.stat().st_mode & 0o777 == 0o700 and (not self.live or (state.stat().st_uid == 0 and state.stat().st_gid == 0)),
                    '维护目录权限与安装契约不符，未调整已有权限。')
        else:
            state.mkdir(parents=True, mode=0o700)
            atomic_write(marker, OWNER)
        for suffix, body in definitions.items():
            path = self.path('/etc/systemd/system/chain-maintenance.' + suffix)
            atomic_write(path, body, 0o644)
        self.system.call('daemon-reload')
        self.system.call('enable', '--now', 'chain-maintenance.socket')

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
        kind = service_kind()
        log = str(self.role_paths(role)[1] / 'service.log') if kind == 'openrc' else f'/var/log/chain-{role}.log'
        command = (f'journalctl -u chain-{role} -n 100 --no-pager' if kind == 'systemd'
                   else 'tail -n 100 ' + shlex.quote(log))
        raise ValueError(f'chain-{role} 启动检查未通过；请查看日志：{command}')

    def check_panel_port(self, env):
        host, port = listen_address(env['CHAIN_LISTEN'])
        with socket.socket(socket.AF_INET6 if ':' in host else socket.AF_INET, socket.SOCK_STREAM) as check:
            check.bind((host, port))

    def backup_files(self, role):
        etc, data, _ = self.role_paths(role)
        directory = self.path('/var/backups/chain-panel')
        directory.mkdir(parents=True, exist_ok=True, mode=0o700)
        name = f'{role}-{datetime.datetime.now(datetime.timezone.utc):%Y%m%dT%H%M%SZ}-{uuid.uuid4().hex[:8]}.tar.gz'
        destination = directory / name
        with destination.open('xb') as stream:
            try:
                os.fchmod(stream.fileno(), 0o600)
                with tarfile.open(fileobj=stream, mode='w:gz') as bundle:
                    for prefix, source in [('config', etc), ('data', data)]:
                        if source.exists():
                            for path in sorted(source.rglob('*')):
                                if path.is_symlink():
                                    require(prefix == 'data' and path.relative_to(source).parts[:3] == ('acme', 'config', 'live')
                                            and not path.readlink().is_absolute()
                                            and path.resolve().is_relative_to(source / 'acme/config/archive') and path.is_file(),
                                            '备份路径包含非 Certbot 内部证书链接，拒绝继续。')
                                if path.is_file() and path.name not in ('xray.log', 'service.log', 'process.lock', 'candidate.json'):
                                    bundle.add(path, arcname=prefix + '/' + str(path.relative_to(source)), recursive=False)
            except BaseException:
                destination.unlink(missing_ok=True)
                raise
        print(f'私有备份：{destination}（权限 600，含连接凭据，请私下保存）')
        return destination

    def recoverable_openrc_data(self, role):
        data = self.role_paths(role)[1]
        if service_kind() != 'openrc' or not self.state_file.is_file() or not data.is_dir():
            return False
        try:
            user = pwd.getpwnam('chain-' + role) if self.live else None
        except KeyError:
            return False
        uid, gid = (user.pw_uid, user.pw_gid) if user else (os.geteuid(), os.getegid())
        def owned(path, mode, owner, group):
            if path.is_symlink() or not path.exists():
                return False
            info = path.stat()
            return (info.st_uid, info.st_gid, info.st_mode & 0o777) == (owner, group, mode)
        if not owned(self.state_file, 0o600, os.geteuid(), os.getegid()) or not owned(data, 0o700, uid, gid):
            return False
        marker, log = data / '.chain-owner', data / 'service.log'
        if marker.is_file() and owned(marker, 0o600, uid, gid):
            return marker.read_text() == 'chain-' + role
        # v0.2.25–27 created this log before the application could claim its empty directory.
        return list(data.iterdir()) == [log] and log.is_file() and owned(log, 0o600, uid, gid)

    def install(self, source, manifest, args):
        role = args.component
        state = self.state()
        old = state['roles'].get(role)
        retained = state.get('retained_roles', {}).get(role)
        require(args.action != 'update' or old, f'尚未安装 {role}；首次请用 install。')
        compatible_release(manifest)
        etc, data, unit = self.role_paths(role)
        wrapper = self.path('/usr/local/bin/chainctl')
        if not state['roles']:
            require(not wrapper.exists(), f'{wrapper} 已存在且不归本安装器管理。')
        if old is None:
            require(not unit.exists(), f'{unit} 已存在，拒绝覆盖其他安装方式创建的服务。')
            for directory in (etc, data):
                require(not directory.is_symlink(), f'{directory} 不能是符号链接。')
                require(retained is not None or not directory.exists() or not any(directory.iterdir())
                        or (directory == data and self.recoverable_openrc_data(role)),
                        f'{directory} 非空，拒绝接管。')
        else:
            require(unit.is_file() and OWNER in unit.read_text(), '服务文件归属不匹配。')
        env, token = self.config(role, args)
        old_env = parse_env(etc / (role + '.env'))
        check_after_stop = False
        if self.live and role == 'panel' and (not self.system.active(role) or old_env.get('CHAIN_LISTEN') != env['CHAIN_LISTEN']):
            check_after_stop = bool(old and self.system.active(role) and
                                    listen_address(old_env['CHAIN_LISTEN'])[1] == listen_address(env['CHAIN_LISTEN'])[1])
            if not check_after_stop:
                self.check_panel_port(env)  # Never stop a foreign port owner.
        uid, gid = self.account(role)
        from chain_network import prepare_proxy_acme_directory
        # systemd needs this writable path even while certificate setup is deferred.
        prepare_proxy_acme_directory(self.path('/var/lib/chain-acme') / role, uid, gid)
        if self.live and not getattr(args, 'defer_proxy_acme', False):
            from chain_network import install_proxy_acme
            if install_proxy_acme(role, uid, gid) is False:
                env.pop('CHAIN_ACME_WEBROOT', None)
        release_id = f'{manifest["version"]}-{manifest.get("os", "linux")}-{manifest["arch"]}-{sha256(source / "release.json")[:12]}'
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
                if old or retained is not None:
                    if old:
                        self.system.call('stop', 'chain-' + role + '.service')
                    self.backup_files(role)
                if check_after_stop:
                    self.check_panel_port(env)
                for directory in (etc, data):
                    directory.mkdir(parents=True, exist_ok=True, mode=0o700)
                    directory.chmod(0o700)
                    os.chown(directory, uid, gid)
                marker = data / '.chain-owner'
                if service_kind() == 'openrc' and not marker.exists():
                    require(not any(data.iterdir()) or self.recoverable_openrc_data(role),
                            f'{data} 不归本安装器管理，拒绝写入归属标记。')
                    # Claim the directory before supervise-daemon opens service.log.
                    atomic_write(marker, 'chain-' + role)
                    os.chown(marker, uid, gid)
                write_env(etc / (role + '.env'), env)
                if token is not None:
                    atomic_write(etc / 'token', token + '\n')
                for path in configs[:2]:
                    if path.exists():
                        os.chown(path, uid, gid)
                atomic_write(unit, self.unit(role), 0o755 if service_kind() == 'openrc' else 0o644)
                self.link(role, release_id)
                self.system.call('daemon-reload')
                self.system.call('enable', 'chain-' + role + '.service')
                self.system.call('restart', 'chain-' + role + '.service')
                self.healthy(role, env)
                state['roles'][role] = {'current': release_id, 'previous': old['current'] if old and old['current'] != release_id else (old or {}).get('previous'),
                                        'version': manifest['version']}
                if role == 'panel':
                    state['roles'][role]['setup_complete'] = (False if args.action == 'install'
                                                             else (old or {}).get('setup_complete', True))
                    state['roles'][role]['manual_https'] = (old or retained or {}).get('manual_https')
                    state['roles'][role]['https_port'] = (old or retained or {}).get('https_port')
                    state['roles'][role]['panel_only'] = (getattr(args, 'panel_only', False) if args.action == 'install'
                                                          else (old or {}).get('panel_only', False))
                state.get('retained_roles', {}).pop(role, None)
                json_write(self.state_file, state)
                manager = self.base / 'manager'
                manager.mkdir(exist_ok=True)
                for name in ['chain_install.py', 'chain_common.py', 'chain_network.py', 'chain_ingress.py', 'chain_platform.py', 'chain_legacy.py', 'chain_time.py']:
                    if name in ('chain_legacy.py', 'chain_time.py') and not (release / 'scripts' / name).exists():
                        continue
                    if name == 'chain_platform.py' and 'os' not in manifest and not (release / 'scripts' / name).exists():
                        continue
                    atomic_write(manager / name, (release / 'scripts' / name).read_bytes(), 0o644)
                atomic_write(wrapper, f'#!/bin/sh\n# {OWNER}\nset -eu\nexec {shlex.quote(sys.executable)} -B {shlex.quote(str(manager / "chain_install.py"))} "$@"\n', 0o755)
            except BaseException:
                self.system.call('stop', 'chain-' + role + '.service', check=False)
                for path, value in previous.items():
                    if value is None:
                        path.unlink(missing_ok=True)
                    else:
                        atomic_write(path, value, (0o755 if service_kind() == 'openrc' else 0o644) if path == unit else 0o600)
                        if path != unit:
                            os.chown(path, uid, gid)
                if old:
                    self.link(role, old['current'])
                    state['roles'][role] = old
                else:
                    (self.base / ('current-' + role)).unlink(missing_ok=True)
                    state['roles'].pop(role, None)
                if retained is not None:
                    state.setdefault('retained_roles', {})[role] = retained
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
                if not getattr(args, 'defer_access', False):
                    self.show_access()
            else:
                print('Agent 已启动，可在 Web「代理」中新建本机代理。排障时输入 chainctl，选择「查看日志」。')
        else:
            print(f'离线安装树已准备：{self.root}；未创建系统账户、未操作服务管理器、未启动监听服务。')

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
        elif not token.is_file():
            print('管理员令牌文件尚未生成；请检查控制台是否启动成功，并查看服务日志。')
        else:
            print('当前不是交互终端；请在 SSH 终端运行 chainctl access 查看登录令牌。')
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
                state['roles'][role] = {**current, 'current': prior, 'previous': current['current'], 'version': manifest['version']}
                json_write(self.state_file, state)
            except BaseException:
                self.system.call('stop', 'chain-' + role + '.service', check=False)
                self.link(role, current['current'])
                if active:
                    self.system.call('start', 'chain-' + role + '.service')
                raise
        print('程序版本已回滚，业务数据与配置保留；没有将数据库恢复到旧备份。')

    def uninstall(self, args):
        state = self.state()
        roles = {**state.get('retained_roles', {}), **state['roles']}
        require(roles, '没有本安装器管理的组件可卸载。')
        require(set(roles) <= set(ROLES), '安装状态中的组件无效。')
        purge = args.purge
        print('将卸载本机全部 Chain Panel / Agent、服务和 chainctl；' +
              ('删除配置、数据库、备份和服务账户。' if purge else '保留配置、数据库、备份和服务账户，供重装恢复。'))
        print('保留系统 Nginx、其他网站和 /etc/letsencrypt 中的面板证书。')
        if not args.non_interactive:
            word = 'PURGE' if purge else 'UNINSTALL'
            require(input(f'输入 {word} 确认: ').strip() == word, '已取消卸载。')
        from chain_ingress import Ingress, OWNER as INGRESS_OWNER
        ingress = Ingress(self.root)
        wrapper = self.path('/usr/local/bin/chainctl')
        hook, renew_service, renew_timer = [self.path(str(p)) for p in HttpsSite('uninstall.invalid', 0).renewal_paths()]
        units = [(self.role_paths(role)[2], 'chain-' + role + '.service', OWNER) for role in roles]
        units.append((self.path(str(service_path('chain-panel-https'))), 'chain-panel-https.service', SITE_OWNER))
        if service_kind() == 'systemd':
            units += [(renew_service, 'chain-panel-cert-renew.service', SITE_OWNER),
                      (renew_timer, 'chain-panel-cert-renew.timer', SITE_OWNER)]
            units += [(self.path('/etc/systemd/system/chain-maintenance.' + suffix),
                       'chain-maintenance.' + suffix, OWNER) for suffix in ('socket', 'service')]
        else:
            units.append((renew_timer, 'chain-panel-cert-renew', SITE_OWNER))
        units += [(p, p.name if service_kind() == 'systemd' else 'chain-ingress', INGRESS_OWNER)
                  for p in ingress.unit_paths()]
        config_root = nginx_conf().parent if self.live else self.path('/etc/nginx')
        sites = [p for name in ('chain-panel.conf', 'chain-proxy-acme.conf') for p in config_root.rglob(name)]
        files = [(wrapper, OWNER), (hook, SITE_OWNER), (renew_service, SITE_OWNER),
                 (self.path('/etc/chain-panel-https/nginx.conf'), SITE_OWNER),
                 *[(p, owner) for p, _, owner in units], *[(p, SITE_OWNER) for p in sites]]
        https_temporary = self.path('/var/lib/chain-panel-https')
        https_marker = https_temporary / '.chain-owner'
        require(not https_temporary.is_symlink() and not os.path.ismount(https_temporary),
                f'拒绝删除链接或挂载目录：{https_temporary}')
        require(not https_temporary.exists() or (https_temporary.is_dir() and not https_marker.is_symlink()
                and https_marker.is_file() and https_marker.read_text().strip() == SITE_OWNER),
                f'{https_temporary} 不归本安装器管理，拒绝删除。')
        directories = [self.base, ingress.directory, ingress.config, self.path('/var/lib/chain-acme')]
        directories += [p for role in roles for p in self.role_paths(role)[:2]]
        backups = self.path('/var/backups/chain-panel')
        directories.append(backups)
        maintenance_state = self.path('/var/lib/chain-maintenance')
        directories.append(maintenance_state)
        legacy_assets = self.path('/var/lib/chain-legacy-assets')
        directories.append(legacy_assets)
        if legacy_assets.exists():
            marker = legacy_assets / '.chain-owner'
            require(marker.is_file() and not marker.is_symlink() and marker.read_text() == 'chain-legacy-assets-v1',
                    '旧配置地理数据目录归属不匹配，拒绝删除。')
        if maintenance_state.exists():
            marker = maintenance_state / '.chain-owner'
            require(marker.is_file() and not marker.is_symlink() and marker.read_text() == OWNER,
                    '维护目录归属不匹配，拒绝删除。')
        for p in directories:
            require(not p.is_symlink() and not os.path.ismount(p), f'拒绝删除链接或挂载目录：{p}')
        for p, owner in files:
            require(not p.is_symlink() and (not p.exists() or p.is_file() and owner in p.read_text()),
                    f'{p} 不归本安装器管理，拒绝删除。')
        logs = [self.path('/var/log/chain-' + role + '.log') for role in roles] + [self.path('/var/log/chain-panel-https.log')]
        if service_kind() == 'openrc':
            logs += [self.role_paths(role)[1] / 'service.log' for role in roles]
        for p in logs:
            require(not p.is_symlink(), f'日志路径为链接，拒绝删除：{p}')
        if purge and backups.exists():
            require(all(p.is_file() and not p.is_symlink() and
                        re.fullmatch(r'(panel|agent)-\d{8}T\d{6}Z-[a-f0-9]{8}\.tar\.gz', p.name)
                        for p in backups.iterdir()), '备份目录包含其他文件，拒绝清除。')
        if 'panel' in roles:
            env = parse_env(self.role_paths('panel')[0] / 'panel.env')
            url = urllib.parse.urlsplit(env.get('CHAIN_PUBLIC_URL', ''))
            if url.scheme == 'https' and url.hostname:
                domain = domain_name(url.hostname)
                if roles['panel'].get('manual_https') or any(p.name == 'chain-panel.conf' for p in sites) or self.path(
                        '/etc/letsencrypt/live/chain-panel-' + domain + '/fullchain.pem').is_file():
                    roles['panel'] = {**roles['panel'], 'domain': domain}
        with self.lock():
            require(self.state() == state, '安装状态已变化，请重试。')
            for p, name, _ in units:
                if p.exists():
                    self.system.call('stop', name)
                    self.system.call('disable', name)
            for role in roles:
                require(not self.system.active(role), f'chain-{role} 尚未停止，未删除数据。')
            if ingress.state_path.exists():
                require(self.live, '离线安装树含统一入口状态，请在原系统关闭统一入口后卸载。')
                ingress.operate('disable')
            previous = {p: (p.read_bytes(), p.stat().st_mode & 0o777) for p in sites}
            try:
                for p in sites:
                    p.unlink()
                if sites and self.live and shutil.which('nginx'):
                    run(['nginx', '-t'])
                    if service_state('nginx')[0]:
                        service('reload', 'nginx')
            except BaseException:
                for p, (body, mode) in previous.items():
                    atomic_write(p, body, mode)
                raise
            json_write(self.state_file, {'owner': OWNER, 'roles': {}, 'retained_roles': roles})
            for p, _ in files:
                p.unlink(missing_ok=True)
            with contextlib.suppress(FileNotFoundError, OSError):
                self.path('/etc/chain-panel-https').rmdir()
            self.system.call('daemon-reload')
            if purge and self.live:
                for role in roles:
                    remove_account('chain-' + role)
            remove = [ingress.directory, ingress.config, self.path('/var/lib/chain-acme'), https_temporary]
            if purge:
                remove += [p for role in roles for p in self.role_paths(role)[:2]] + [backups, maintenance_state, legacy_assets]
            for p in remove:
                if p.exists():
                    shutil.rmtree(p)
            for p in logs:
                p.unlink(missing_ok=True)
            for p in self.base.iterdir():
                if p.name not in ('.chain-install.json', '.install.lock'):
                    if p.is_dir() and not p.is_symlink():
                        shutil.rmtree(p)
                    else:
                        p.unlink()
        if purge:
            shutil.rmtree(self.base)
        print('已卸载；' + ('应用数据已清除。' if purge else '数据已保留，重新运行安装脚本可恢复使用。') + '面板证书保留。')

    def operate(self, args):
        if args.action == 'uninstall':
            return self.uninstall(args)
        role, action = args.component, args.action
        state = self.require_role(role)
        if action == 'status':
            print(f'{ROLE_NAMES[role]} · 版本 {state["roles"][role]["version"]}')
            self.system.call('status', 'chain-' + role + '.service', '--no-pager', check=False)
        elif action == 'logs':
            require(self.live, '--root 离线安装树没有服务日志。')
            source = getattr(args, 'log_source', 'service')
            follow = getattr(args, 'follow', False)
            data = self.role_paths(role)[1]
            if source == 'service' and service_kind() == 'systemd':
                command = ['journalctl', '-u', 'chain-' + role + '.service', '--no-pager', '-n', '100']
                if follow:
                    command.append('-f')
            else:
                logs = {'application': data / 'diagnostics.log', 'xray': data / 'xray.log',
                        'mihomo': data / 'mihomo' / 'mihomo.log',
                        'service': data / 'service.log' if service_kind() == 'openrc' else Path('/var/log/chain-' + role + '.log')}
                if source.startswith('legacy-'):
                    import uuid
                    require(str(uuid.UUID(source[7:])) == source[7:], '迁移配置 ID 无效。')
                    logs[source] = data / source / 'xray.log'
                require(source in logs, '日志来源无效。')
                log = logs[source]
                require(not log.is_symlink() and not log.parent.is_symlink(), '拒绝读取日志符号链接。')
                require(log.is_file() or follow, '该日志尚未产生；可添加 --follow 等待。')
                command = ['tail', '-n', '100'] + (['-F'] if follow else []) + [str(log)]
            print('持续查看日志，Ctrl+C 返回。' if follow else '最近 100 行；添加 --follow 持续查看。')
            try:
                run(command)
            except KeyboardInterrupt:
                print('已停止跟随日志。')
        elif action == 'token':
            require(role == 'panel', 'Agent 令牌只在 Web 创建或重置时显示。')
            print((self.role_paths(role)[1] / 'admin-token').read_text().strip())
        elif action == 'access':
            self.show_access()
        elif action == 'reset-auth':
            require(role == 'panel', '登录安全恢复仅用于控制台。')
            if not args.non_interactive:
                require(input('此操作将清除 TOTP、Passkey 并退出全部会话，保留当前管理员密钥。输入 RESET 确认: ').strip() == 'RESET', '已取消恢复。')
            with self.lock():
                active = self.system.active(role)
                self.system.call('stop', 'chain-panel.service')
                try:
                    self.backup_files(role)
                    database = self.role_paths(role)[1] / 'panel.db'
                    require(database.is_file(), '控制台数据库不存在。')
                    with contextlib.closing(sqlite3.connect(database)) as connection, connection:
                        row = connection.execute('SELECT body FROM state WHERE id=1').fetchone()
                        if row:
                            document = json.loads(row[0])
                            document['security'] = {}
                            connection.execute('UPDATE state SET body=? WHERE id=1', (json.dumps(document),))
                finally:
                    if active:
                        self.system.call('start', 'chain-panel.service')
            print('登录二步验证和 Passkey 已重置；使用原管理员密钥登录。业务线路未改动。')
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
    return sorted((ROOT / 'release-output').glob(f'*/chain-panel-*-{operating_system()}-{architecture()}.tar.gz'),
                  key=lambda p: p.stat().st_mtime)


def provision_local_agent(manager, source, manifest, args):
    state = manager.state()
    if 'agent' in state['roles'] or 'agent' in state.get('retained_roles', {}):
        agent_env = parse_env(manager.role_paths('agent')[0] / 'agent.env')
        panel_env = parse_env(manager.role_paths('panel')[0] / 'panel.env')
        local = False
        if 'CHAIN_LISTEN' in panel_env:
            agent_url = urllib.parse.urlsplit(agent_env.get('CHAIN_PANEL_URL', ''))
            panel_host, panel_port = listen_address(panel_env['CHAIN_LISTEN'])
            local_host = {'0.0.0.0': '127.0.0.1', '::': '::1'}.get(panel_host, panel_host)
            same_host = agent_url.hostname == local_host or (
                agent_url.hostname == 'localhost' and ipaddress.ip_address(local_host).is_loopback)
            local = agent_url.scheme == 'http' and same_host and agent_url.port == panel_port
        if local or (args.action == 'update' and 'agent' in state['roles']):
            action = 'update' if 'agent' in state['roles'] else 'install'
            agent = make_parser().parse_args([action, '--component', 'agent', '--root', str(manager.root), '--non-interactive'])
            agent.allow_http = local  # Preserve remote endpoints' existing HTTPS / HTTP consent.
            agent.defer_proxy_acme = getattr(args, 'defer_proxy_acme', False)
            manager.install(source, manifest, agent)
            if local:
                mark_local_node(manager, agent_env['CHAIN_NODE_ID'])
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
        agent.defer_proxy_acme = getattr(args, 'defer_proxy_acme', False)
        manager.install(source, manifest, agent)
        mark_local_node(manager, node['id'])
        print('本机 Agent 已安装并自动接入；在 Web「代理」中直接新建代理即可。')
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


def mark_local_node(manager, node_id):
    uuid.UUID(node_id)
    path = manager.role_paths('panel')[1] / 'local-system-node'
    atomic_write(path, node_id + '\n')
    token_file = manager.role_paths('agent')[0] / 'token'
    if token_file.is_file():
        recovery = path.with_name('local-system-identity.json')
        json_write(recovery, {'id': node_id, 'token_hash': hashlib.sha256(token_file.read_text().strip().encode()).hexdigest()})
        if manager.live:
            uid, gid = manager.account('panel')
            os.chown(recovery, uid, gid)
    if manager.live:
        uid, gid = manager.account('panel')
        os.chown(path, uid, gid)


def install_components(manager, source, manifest, args):
    manager.install(source, manifest, args)
    if manager.live and args.component == 'panel' and (
            (args.action == 'install' and not getattr(args, 'panel_only', False)) or
            (args.action == 'update' and 'agent' in manager.state()['roles'])):
        provision_local_agent(manager, source, manifest, args)


def install_package(manager, args):
    if args.action == 'update':
        manager.require_role(args.component)
    # Fix the clock before HTTPS downloads and enrollment can fail on certificate dates.
    if manager.live and service_kind() == 'systemd' and args.action in ('install', 'update'):
        from chain_time import status, synchronize
        try:
            clock = status()
            if clock.get('NTPSynchronized') != 'yes' or clock.get('NTP') != 'yes':
                print('自动检查并启用时间同步，必要时安装时间服务；保持现有时区。')
                synchronize()
        except (ValueError, OSError, subprocess.SubprocessError) as error:
            print(f'时间同步未完成：{error}；继续组件操作，连接可能因时钟偏差失败。')

    def install_source(source, manifest):
        # Validate the downloaded package before asking for deployment details.
        compatible_release(manifest)
        if args.action == 'install' and args.component == 'panel':
            state = manager.state()
            retained = state['roles'].get('panel') or state.get('retained_roles', {}).get('panel', {})
            if args.domain is None:
                args.domain = retained.get('domain')
                if args.domain is None and manager.panel_setup_pending():
                    env = parse_env(manager.role_paths('panel')[0] / 'panel.env')
                    url = urllib.parse.urlsplit(env.get('CHAIN_PUBLIC_URL', ''))
                    if url.scheme == 'https':
                        args.domain = url.hostname
            args.panel_only = args.panel_only or retained.get('panel_only', False)
        if args.action == 'install' and args.component == 'panel' and (
                getattr(args, 'guided', False) or getattr(args, 'domain', None) is not None):
            panel_questions(manager, args)
        elif args.action == 'install' and args.component == 'agent' and getattr(args, 'guided', False):
            agent_questions(manager, args)
        if manager.live and args.action == 'install' and args.component == 'panel' and getattr(args, 'guided', False):
            from chain_legacy import guided_install
            if guided_install(manager, source, manifest, args):
                manager.maintenance_units()
                manager.complete_panel_setup()
                manager.show_access()
                return
        site = None
        if getattr(args, 'domain', None):
            require(manager.live, '自动 HTTPS 仅用于实际系统安装，不支持 --root 离线模式。')
            site = HttpsSite(args.domain, listen_address(args.listen)[1], getattr(args, 'domain_ipv6', False))
            saved = manager.state()
            previous_panel = saved['roles'].get('panel') or saved.get('retained_roles', {}).get('panel', {})
            site.https_port = previous_panel.get('https_port') or 443
            if site.https_port != 443:
                args.public_url = f'https://{args.domain}:{site.https_port}'
            manual = previous_panel.get('manual_https')
            if manual:
                site.https_port, site.certificate, site.key = manual['port'], Path(manual['cert']), Path(manual['key'])
                args.public_url = f'https://{args.domain}:{site.https_port}'
        args.defer_access = True
        with site.activate() if site else contextlib.nullcontext():
            manager.install(source, manifest, args)
        # HTTPS and the panel are now committed. An Agent failure must not undo
        # the working panel URL; rerunning setup retries the missing local node.
        if manager.live and args.component == 'panel' and (
                (args.action == 'install' and not args.panel_only) or
                (args.action == 'update' and 'agent' in manager.state()['roles'])):
            try:
                provision_local_agent(manager, source, manifest, args)
            except (ValueError, OSError, subprocess.SubprocessError):
                if args.action == 'update':
                    print('部分更新完成：控制台与 chainctl 已更新，本机 Agent 更新未完成；修复错误后重新选择更新。', file=sys.stderr)
                raise
        if manager.live and args.component == 'panel':
            manager.maintenance_units()
            if args.action == 'install':
                manager.complete_panel_setup()
            manager.show_access()
            if site:
                print('HTTPS 已配置，证书由原工具续期。' if site.certificate else 'HTTPS 与证书自动续期已配置。')
            else:
                env = parse_env(manager.role_paths('panel')[0] / 'panel.env')
                if env.get('CHAIN_PUBLIC_URL', '').startswith('http://'):
                    print(f'如无法访问，请在云安全组/防火墙放行 TCP {listen_address(env["CHAIN_LISTEN"])[1]}。')
        elif args.component == 'panel' and args.action == 'install':
            manager.complete_panel_setup()

    if getattr(args, 'reuse_current', False):
        source = (manager.base / ('current-' + args.component)).resolve(strict=True)
        install_source(source, verify_tree(source))
        return
    latest = getattr(args, 'latest', False) or (args.action == 'update' and not args.package)
    if not args.package and not latest and (ROOT / 'release.json').is_file():
        install_source(ROOT, verify_tree(ROOT))
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
        install_source(stage, manifest)


def choose(title, options, default='1'):
    while True:
        print('\n' + title)
        for key, label in options.items():
            print(f'{key}) {label}')
        prompt = f'输入编号后回车 [{default}]: '
        if readline is None and sys.stdin.isatty():
            value = getpass.getpass('当前终端不支持行编辑，编号输入不回显；' + prompt)
            value = re.sub(r'\x1b(?:\[[0-?]*[ -/]*[@-~]|O[@-~])', '', value)
        else:
            value = input(prompt)
        value = value.strip() or default
        if value in options:
            return value
        print('请输入上面的选项编号。')


def panel_questions(manager, args):
    etc, _, _ = manager.role_paths('panel')
    prior = parse_env(etc / 'panel.env')
    if (args.listen or prior) and getattr(args, 'domain', None) is None:
        args.listen = args.listen or prior.get('CHAIN_LISTEN')
        args.public_url = args.public_url or prior.get('CHAIN_PUBLIC_URL')
        if args.secure_cookie is None:
            args.secure_cookie = prior.get('CHAIN_SECURE_COOKIE')
        return
    host = args.local_host or public_ipv4()
    if not host:
        require(getattr(args, 'guided', False), '网卡未配置公网 IPv4；NAT 环境请通过 --local-host 指定公网入站地址。')
        while True:
            host = input('网卡没有公网 IPv4（可能位于 NAT 后），请输入公网入站 IPv4: ').strip()
            try:
                ipaddress.IPv4Address(host)
                break
            except ValueError:
                print('请填写有效 IPv4 地址。')
    args.local_host = host
    print(f'服务器地址：{host}；默认安装控制台与本机节点。')
    explicit = args.domain is not None
    while True:
        value = args.domain if explicit else input('面板域名（可选，直接回车使用 IP 访问）: ').strip()
        if not value:
            args.domain = None
            break
        try:
            domain = domain_name(value)
            resolved = validate_dns(domain, server_addresses(host))
            args.domain, args.domain_ipv6 = domain, any(':' in address for address in resolved)
            break
        except (ValueError, OSError) as error:
            if explicit:
                raise
            print(error)
            print('修改解析后重新输入域名；也可直接回车使用 IP 访问。')
    retained_listen = args.listen or prior.get('CHAIN_LISTEN')
    port = listen_address(retained_listen)[1] if retained_listen else available_port()
    args.listen = f'{"127.0.0.1" if args.domain else "0.0.0.0"}:{port}'
    args.secure_cookie = '1' if args.domain else '0'
    args.public_url = f'https://{args.domain}' if args.domain else f'http://{host}:{port}'
    print(f'{"保留控制台端口" if retained_listen else "已自动选择空闲随机端口"}：{port}。')
    if not args.domain:
        print('使用 IP 直接访问（HTTP）；浏览器到面板的通信不加密。')


def agent_questions(manager, args):
    if args.panel_url and args.node_id and args.token_file:
        return
    state = manager.state()
    installed = 'agent' in state['roles'] or 'agent' in state.get('retained_roles', {})
    print('在 Web「服务器」中添加节点或重置接入码，然后复制完整接入码。')
    while True:
        value = getpass.getpass('粘贴接入码（不回显，输入 0 返回）' + ('，直接回车保留当前身份' if installed else '') + ': ').strip()
        if value == '0':
            raise KeyboardInterrupt
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
    args.guided = True
    if not args.package and not args.latest:
        if args.action == 'install' and args.component in manager.state()['roles']:
            args.reuse_current = True
    install_package(manager, args)


def select_role(manager):
    installed = list(manager.state()['roles'])
    require(installed, '请先从菜单安装控制台或接入节点。')
    if len(installed) == 1:
        return installed[0]
    options = {str(i + 1): ROLE_NAMES[role] for i, role in enumerate(installed)}
    options['0'] = '返回主菜单'
    choice = choose('选择要管理的组件', options)
    return None if choice == '0' else installed[int(choice) - 1]


def menu(root):
    manager = Manager(root)
    while True:
        installed = manager.state()['roles']
        print('\nChain Panel · 管理菜单')
        for role, value in installed.items():
            running = ('运行中' if manager.system.active(role) else '已停止') if manager.live else '离线安装树'
            print(f'  {ROLE_NAMES[role]}  v{value["version"]}  {running}')
        choice = choose('请选择操作', {
            '1': '安装 / 修复控制台与本机节点', '2': '接入 / 重新接入节点', '3': '更新本机全部组件与 chainctl',
            '4': '查看状态', '5': '查看日志', '6': '重启服务', '7': '备份数据',
            '8': '回滚上一版本', '9': '查看登录信息', '10': '启动服务', '11': '停止服务',
            '12': '查看命令帮助', '13': '重置登录安全（TOTP / Passkey）', '14': '启用统一 443（保留已有网站）', '15': '关闭统一 443',
            '16': '卸载本机全部组件', '0': '退出'}, '4' if installed else '1')
        if choice == '0':
            return
        try:
            if choice == '12':
                make_parser().print_help()
                continue
            if choice == '16':
                mode = choose('卸载方式（两种均保留面板 HTTPS 证书）',
                              {'1': '卸载程序，保留配置、数据和备份', '2': '清除程序、配置、数据和备份', '0': '取消'})
                if mode == '0':
                    continue
                args = make_parser().parse_args(['uninstall', '--root', str(root)] + (['--purge'] if mode == '2' else []))
                manager.uninstall(args)
                return
            if choice in ('14', '15'):
                from chain_ingress import Ingress
                require(manager.live, '统一 443 仅用于实际系统安装。')
                impact = '将调整网站 HTTPS 入口以共享 443。' if choice == '14' else '将关闭代理统一 443 入口并恢复网站原配置。'
                if choose(impact, {'1': '继续', '0': '返回主菜单'}, '0') == '0':
                    continue
                Ingress().operate('enable' if choice == '14' else 'disable')
                print('统一 443 已启用。' if choice == '14' else '统一 443 已关闭。')
                continue
            if choice == '3':
                require(installed, '请先从菜单安装控制台或接入节点。')
                role = 'panel' if 'panel' in installed else 'agent'
            else:
                role = 'panel' if choice in ('1', '9', '13') else 'agent' if choice == '2' else select_role(manager)
            action = {'1': 'install', '2': 'install', '3': 'update', '4': 'status', '5': 'logs',
                      '6': 'restart', '7': 'backup', '8': 'rollback', '9': 'access', '10': 'start', '11': 'stop', '13': 'reset-auth'}[choice]
            if role is None:
                continue
            args = make_parser().parse_args([action, '--component', role, '--root', str(root)])
            if action == 'install' and role in installed:
                if choose('将使用当前版本重新安装，保留已有配置并重启服务。',
                          {'1': '继续修复 / 重新接入', '0': '返回主菜单'}, '0') == '0':
                    continue
            if action in ('backup', 'rollback'):
                print('此操作会短暂停止所选组件，完成后恢复服务；Agent 停止期间代理连接会中断。')
            if action == 'logs':
                sources = {'1': '服务', '2': '应用', '0': '返回主菜单'}
                if role == 'agent':
                    sources.update({'3': 'Xray', '4': 'Mihomo', '5': '迁移内核'})
                source = choose('日志来源', sources)
                if source == '0':
                    continue
                args.log_source = {'1': 'service', '2': 'application', '3': 'xray', '4': 'mihomo', '5': 'legacy-'}[source]
                if source == '5':
                    value = input('迁移配置 UUID（输入 0 返回）: ').strip()
                    if value == '0':
                        continue
                    try:
                        args.log_source += str(uuid.UUID(value))
                    except ValueError:
                        raise ValueError('迁移配置 UUID 无效，请从 Web 迁移配置中复制完整 UUID。') from None
                args.follow = choose('日志查看方式',
                                     {'1': '最近 100 行', '2': '持续查看（Ctrl+C 返回）'}, '2') == '2'
            if action in ('install', 'update'):
                install_wizard(manager, args)
            else:
                manager.operate(args)
        except KeyboardInterrupt:
            print('\n已取消操作，返回主菜单。')
        except EOFError:
            return
        except (ValueError, OSError, subprocess.SubprocessError, sqlite3.Error) as error:
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
  logs --component agent --log-source xray --follow   持续查看 Xray 日志
  start / stop / restart    启动 / 停止 / 重启
  backup / rollback         私有备份 / 回滚程序版本
  access / token            查看登录信息 / 仅输出管理令牌
  reset-auth                本机恢复登录，清除 TOTP / Passkey（保留管理员密钥）
  uninstall                 卸载本机全部组件和 chainctl，保留数据与面板证书
  uninstall --purge         同时清除应用配置、数据库和备份；面板证书仍保留
  ingress-enable / ingress-disable   启用 / 关闭统一 443，保留已有 HTTPS 网站
  --component agent         操作节点 Agent，默认操作控制台
  --package 文件.tar.gz      使用本地发行包，旁边需有 SHA256SUMS
  --non-interactive         自动化调用，不打开安装向导

安装脚本同样支持 help 和以上子命令。""")
    parser.add_argument('action', nargs='?', default='menu', choices=['menu', 'setup', 'help', 'install', 'update', 'status', 'start', 'stop', 'restart', 'logs', 'backup', 'rollback', 'access', 'token', 'reset-auth', 'ingress-enable', 'ingress-disable', 'uninstall'])
    parser.add_argument('--follow', '-f', action='store_true', help='持续查看日志，Ctrl+C 返回')
    parser.add_argument('--log-source', default='service', help='service/application/xray/mihomo/legacy-UUID')
    parser.add_argument('--purge', action='store_true', help='卸载时同时删除应用配置、数据、备份和服务账户；保留面板证书')
    parser.add_argument('--component', choices=ROLES, default='panel', help='默认 panel；节点机器选择 agent')
    parser.add_argument('--package', type=Path, help='本地发行包，同目录需有 SHA256SUMS')
    parser.add_argument('--latest', action='store_true', help='下载最新正式版本')
    parser.add_argument('--non-interactive', action='store_true', help='不进行交互提问')
    parser.add_argument('--listen', help='高级：指定监听地址；首次交互安装自动选择空闲随机端口')
    parser.add_argument('--secure-cookie', choices=['0', '1'], help='HTTPS 反向代理设为 1')
    parser.add_argument('--public-url', help='浏览器访问地址；不会自动配置反向代理')
    parser.add_argument('--domain', help='自动配置域名解析检查、Nginx HTTPS 与续期；空字符串使用 IP 访问')
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
    init_path()
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
    if args.action in ('ingress-enable', 'ingress-disable'):
        require(manager.live, '统一 443 仅用于实际系统安装。')
        from chain_ingress import Ingress
        Ingress().operate(args.action.removeprefix('ingress-'))
        return
    if args.action == 'setup':
        installed = manager.state()['roles']
        retry_agent = (args.component == 'panel' and 'panel' in installed and 'agent' not in installed
                       and not installed['panel'].get('panel_only', False) and not args.panel_only and manager.live)
        retry_setup = args.component == 'panel' and manager.panel_setup_pending()
        if args.component in installed and not retry_agent and not retry_setup:
            if args.component == 'panel':
                manager.show_access()
            else:
                args.action = 'status'
                manager.operate(args)
            print('已经安装；升级运行 chainctl update，日常管理运行 chainctl。')
            return
        if retry_setup:
            print('检测到面板安装尚未完成，继续配置访问入口；保留已有数据和节点身份。')
        require(sys.stdin.isatty() or args.non_interactive,
                '首次安装需要交互终端；请在 SSH 终端运行，自动化部署使用 install --non-interactive。')
        args.action = 'install'
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
