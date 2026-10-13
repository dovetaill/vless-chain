"""Install and operate only the independent chain-panel / chain-agent services."""
import argparse
import contextlib
import datetime
import fcntl
import getpass
import grp
import ipaddress
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
                          run, safe_extract, sha256, verify_archive_checksum, verify_tree)

ROOT = Path(__file__).resolve().parents[1]
OWNER = 'chain-panel-installer-v1'
ROLES = ('panel', 'agent')


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
            env['CHAIN_LISTEN'] = args.listen or env.get('CHAIN_LISTEN', '127.0.0.1:8088')
            env['CHAIN_SECURE_COOKIE'] = args.secure_cookie if args.secure_cookie is not None else env.get('CHAIN_SECURE_COOKIE', '0')
            listen_address(env['CHAIN_LISTEN'])
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
            if args.token_file:
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
                print(f'监听：{env["CHAIN_LISTEN"]}；管理令牌：{data / "admin-token"}\n查看令牌：chainctl token')
                if listen_address(env['CHAIN_LISTEN'])[0] in ('127.0.0.1', '::1'):
                    print('默认仅本机访问。可接 HTTPS 反向代理，或从电脑 SSH 转发此端口后访问。')
            else:
                print('Agent 进程已启动；请在 Web 的服务器页面确认心跳，身份或网络错误可用 chainctl logs --component agent 查看。')
        else:
            print(f'离线安装树已准备：{self.root}；未创建系统账户、未操作 systemd、未启动监听服务。')

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
            print(json.dumps(state['roles'][role], ensure_ascii=False, indent=2))
            self.system.call('status', 'chain-' + role + '.service', '--no-pager', check=False)
        elif action == 'logs':
            require(self.live, '--root 离线安装树没有 systemd 日志。')
            run(['journalctl', '-u', 'chain-' + role + '.service', '--no-pager', '-n', '100'])
        elif action == 'token':
            require(role == 'panel', 'Agent 令牌只在 Web 创建或重置时显示。')
            print((self.role_paths(role)[1] / 'admin-token').read_text().strip())
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


def install_package(manager, args):
    if args.package:
        archive = Path(args.package).resolve(strict=True)
    elif (ROOT / 'release.json').is_file():
        manager.install(ROOT, verify_tree(ROOT), args)
        return
    else:
        choices = sorted((ROOT / 'release-output').glob(f'*/chain-panel-*-linux-{architecture()}.tar.gz'), key=lambda p: p.stat().st_mtime)
        require(choices, '未找到发行包。请先运行 ./scripts/version-release.sh build，或使用 --package /路径/发行包.tar.gz。')
        archive = choices[-1]
    verify_archive_checksum(archive)
    parent = Path(args.work_parent).resolve(strict=True) if args.work_parent else manager.root / 'var/tmp'
    parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix='chain-install-', dir=parent) as temporary:
        print(f'安装临时目录：{temporary}；PID={os.getpid()}；无临时监听服务。')
        stage = Path(temporary) / 'stage'
        manifest = safe_extract(archive, stage)
        try:
            manager.install(stage, manifest, args)
        finally:
            print(f'安装暂存文件将在退出时清理：{temporary}')


def menu(root):
    require(sys.stdin.isatty(), '非交互环境请指定 install / update / status 等子命令，或 --help。')
    while True:
        print('\nChain Panel · 安装与运维\n1) 安装 / 更新 Web 控制台（默认）\n2) 安装 / 更新节点 Agent\n'
              '3) 服务状态\n4) 启动\n5) 停止\n6) 重启\n7) 最近日志\n8) 备份\n9) 回滚程序版本\n10) 查看控制台管理令牌\n0) 退出')
        choice = input('选择 [1]: ').strip() or '1'
        if choice == '0':
            return
        action = {'1': 'install', '2': 'install', '3': 'status', '4': 'start', '5': 'stop', '6': 'restart',
                  '7': 'logs', '8': 'backup', '9': 'rollback', '10': 'token'}.get(choice)
        if not action:
            continue
        role = 'agent' if choice == '2' else 'panel'
        if choice not in ('1', '2', '10'):
            role = input('组件 panel / agent [panel]: ').strip() or 'panel'
        command = [action, '--component', role, '--root', str(root)]
        if action == 'install':
            package = input('发行包路径 [自动选择本地最新包]: ').strip()
            if package:
                command += ['--package', package]
            if role == 'panel':
                listen = input('监听地址 [保留配置，首次 127.0.0.1:8088]: ').strip()
                if listen:
                    command += ['--listen', listen]
        try:
            main(command)
        except (ValueError, OSError, subprocess.CalledProcessError) as error:
            print(f'操作未完成：{error}', file=sys.stderr)


def main(argv=None):
    parser = argparse.ArgumentParser(description='安装并管理独立的 Chain Panel / Agent，不接管旧 SSH 脚本服务。')
    parser.add_argument('action', nargs='?', default='menu', choices=['menu', 'install', 'update', 'status', 'start', 'stop', 'restart', 'logs', 'backup', 'rollback', 'token'])
    parser.add_argument('--component', choices=ROLES, default='panel', help='默认 panel；节点机器选择 agent')
    parser.add_argument('--package', type=Path, help='同目录需有 SHA256SUMS')
    parser.add_argument('--listen', help='控制台监听地址，首次默认 127.0.0.1:8088')
    parser.add_argument('--secure-cookie', choices=['0', '1'], help='通过 HTTPS 反向代理访问时设为 1')
    parser.add_argument('--panel-url')
    parser.add_argument('--node-id')
    parser.add_argument('--token-file', type=Path, help='权限 600 的节点令牌文件；不通过命令行传入令牌正文')
    parser.add_argument('--allow-http', action='store_true', help='允许 Agent 在明确可信私网连接 HTTP 控制台')
    parser.add_argument('--work-parent', type=Path, help='安装包临时展开目录的父目录')
    parser.add_argument('--root', type=Path, default=Path('/'), help='生成离线安装树用于审核/测试；非 / 时不操作用户或 systemd')
    args = parser.parse_args(argv)
    if args.action == 'menu':
        return menu(args.root)
    manager = Manager(args.root)
    if args.action in ('install', 'update'):
        install_package(manager, args)
    else:
        manager.operate(args)


if __name__ == '__main__':
    signal.signal(signal.SIGTERM, lambda *_: sys.exit(143))
    try:
        main()
    except (ValueError, OSError, subprocess.CalledProcessError, KeyboardInterrupt) as error:
        raise SystemExit(str(error) or '操作已中断。')
