"""Linux/OpenRC and macOS service integration shared by installation and HTTPS."""
import os
from pathlib import Path
import plistlib
import pwd
import grp
import re
import shlex
import shutil
import subprocess
import sys

from chain_common import atomic_write, output, require, run, operating_system

SERVICE_PATH = '/opt/homebrew/opt/openssl@3/bin:/usr/local/opt/openssl@3/bin:/opt/homebrew/bin:/opt/homebrew/sbin:/usr/local/bin:/usr/local/sbin:/usr/bin:/bin:/usr/sbin:/sbin'


def service_kind():
    if operating_system() == 'darwin':
        return 'launchd'
    if Path('/run/openrc').exists() and not Path('/run/systemd/system').is_dir():
        return 'openrc'
    return 'systemd'


def service_path(name, kind=None):
    kind = kind or service_kind()
    name = name.removesuffix('.service').removesuffix('.timer')
    if kind == 'launchd':
        return Path('/Library/LaunchDaemons') / f'org.vless-chain.{name}.plist'
    if kind == 'openrc':
        return Path('/etc/init.d') / name
    return Path('/etc/systemd/system') / (name + '.service')


def service(action, name, check=True):
    kind = service_kind()
    if kind == 'systemd':
        return run(['systemctl', action, name], check=check).returncode
    name = name.removesuffix('.service').removesuffix('.timer')
    if kind == 'openrc':
        if action in ('enable', 'disable'):
            argv = ['rc-update', 'add' if action == 'enable' else 'del', name, 'default']
        else:
            argv = ['rc-service', name, action]
        return run(argv, check=check).returncode
    label = 'org.vless-chain.' + name
    target = 'system/' + label
    path = service_path(name)
    if action in ('enable', 'disable'):
        return run(['launchctl', action, target], check=check).returncode
    if action == 'status':
        return run(['launchctl', 'print', target], check=check).returncode
    if action == 'reload':
        return run(['launchctl', 'kill', 'SIGHUP', target], check=check).returncode
    if action in ('stop', 'restart'):
        loaded = subprocess.run(['launchctl', 'print', target], stdout=subprocess.DEVNULL,
                                stderr=subprocess.DEVNULL).returncode == 0
        if loaded:
            code = run(['launchctl', 'bootout', target], check=check).returncode
            if code:
                return code
        if action == 'stop':
            return 0
    if action in ('start', 'restart'):
        loaded = subprocess.run(['launchctl', 'print', target], stdout=subprocess.DEVNULL,
                                stderr=subprocess.DEVNULL).returncode == 0
        if not loaded:
            return run(['launchctl', 'bootstrap', 'system', path], check=check).returncode
        return run(['launchctl', 'kickstart', target], check=check).returncode
    raise ValueError('不支持的服务操作：' + action)


def service_state(name):
    kind = service_kind()
    if kind == 'systemd':
        return tuple(subprocess.run(['systemctl', action, '--quiet', name], stdout=subprocess.DEVNULL,
                                    stderr=subprocess.DEVNULL).returncode == 0
                     for action in ('is-active', 'is-enabled'))
    name = name.removesuffix('.service').removesuffix('.timer')
    if kind == 'openrc':
        active = subprocess.run(['rc-service', name, 'status'], stdout=subprocess.DEVNULL,
                                stderr=subprocess.DEVNULL).returncode == 0
        return active, (Path('/etc/runlevels/default') / name).exists()
    target = 'system/org.vless-chain.' + name
    state = subprocess.run(['launchctl', 'print', target], capture_output=True, text=True)
    disabled = output(['launchctl', 'print-disabled', 'system'])
    enabled = service_path(name).exists() and not re.search(
        r'"' + re.escape(target.removeprefix('system/')) + r'"\s*=>\s*(true|disabled)', disabled)
    active = state.returncode == 0 and (name == 'chain-panel-cert-renew' or bool(re.search(r'\bpid = \d+', state.stdout)))
    return active, bool(enabled)


def service_definition(name, command, owner, user=None, env=None, interval=None):
    """Generate owned non-systemd services; environment values are never shell-evaluated."""
    env = {'PATH': SERVICE_PATH, **(env or {})}
    log = '/var/log/' + name + '.log'
    if service_kind() == 'launchd':
        value = {'Label': 'org.vless-chain.' + name, 'Comment': owner,
                 'ProgramArguments': list(map(str, command)), 'EnvironmentVariables': env,
                 'RunAtLoad': True, 'ThrottleInterval': 5, 'ExitTimeOut': 50,
                 'AbandonProcessGroup': False, 'Umask': 0o077,
                 'StandardOutPath': log, 'StandardErrorPath': log}
        if user:
            value.update(UserName=user, GroupName=user)
        if name == 'chain-agent':
            limit = min(65536, int(output(['sysctl', '-n', 'kern.maxfilesperproc'])))
            value['SoftResourceLimits'] = {'NumberOfFiles': limit}
            value['HardResourceLimits'] = {'NumberOfFiles': limit}
        if interval:
            value['StartInterval'] = interval
        else:
            value['KeepAlive'] = {'SuccessfulExit': False}
        return plistlib.dumps(value).decode()
    if interval:
        command = ['/bin/sh', '-c', 'while :; do ' + shlex.join(list(map(str, command))) +
                   '; sleep ' + str(interval) + '; done']
    text = (f'#!/sbin/openrc-run\n# {owner}\nsupervisor=supervise-daemon\n'
            f'command={shlex.quote(str(command[0]))}\n'
            f'command_args={shlex.quote(shlex.join(list(map(str, command[1:]))))}\n'
            'respawn_delay=5\nrespawn_max=0\nretry="TERM/50/KILL/5"\nstopgroup=yes\numask=0077\n'
            f'output_log={shlex.quote(log)}\nerror_log={shlex.quote(log)}\n')
    if user:
        text += f'command_user={shlex.quote(user + ":" + user)}\nno_new_privs=yes\n'
        if name == 'chain-agent':
            text += 'capabilities="^cap_net_bind_service"\nrc_ulimit="-n 65536"\n'
    text += ''.join(f'export {key}={shlex.quote(value)}\n' for key, value in env.items())
    return text + 'depend() { need net; }\n'


def create_account(name, home):
    if operating_system() == 'darwin':
        ids = {entry.pw_uid for entry in pwd.getpwall()} | {entry.gr_gid for entry in grp.getgrall()}
        uid = next((value for value in range(499, 199, -1) if value not in ids), None)
        require(uid, '没有可用的 macOS 服务账户 ID。')
        group, user = '/Groups/' + name, '/Users/' + name
        for record, attributes in [(group, {'PrimaryGroupID': str(uid)}),
                                   (user, {'UniqueID': str(uid), 'PrimaryGroupID': str(uid),
                                           'NFSHomeDirectory': home, 'UserShell': '/usr/bin/false',
                                           'IsHidden': '1', 'Password': '*'})]:
            run(['dscl', '.', '-create', record])
            for key, value in attributes.items():
                run(['dscl', '.', '-create', record, key, value])
    elif shutil.which('useradd'):
        run(['useradd', '--system', '--user-group', '--no-create-home', '--home-dir', home,
             '--shell', shutil.which('nologin') or '/bin/false', name])
    else:
        run(['addgroup', '-S', name])
        run(['adduser', '-S', '-D', '-H', '-h', home, '-s', '/sbin/nologin', '-G', name, name])


def brew_command(*args):
    brew = shutil.which('brew')
    require(brew, 'macOS 请先通过 https://brew.sh 安装 Homebrew。')
    user = os.environ.get('SUDO_USER')
    require(user and user != 'root', 'macOS 请从普通管理员账户通过 sudo 运行安装器，以该用户安装 Homebrew 依赖。')
    return ['sudo', '-H', '-u', user, brew, *args]


def nginx_conf():
    if operating_system() == 'darwin':
        return Path(output(brew_command('--prefix'))) / 'etc/nginx/nginx.conf'
    return Path('/etc/nginx/nginx.conf')


def ensure_nginx_service(owner):
    if service_kind() != 'launchd':
        return
    # A user LaunchAgent cannot manage the root-owned certificate and listener configuration.
    processes = output(['ps', '-axo', 'uid=,command='])
    require(not any('nginx: master process' in line and line.split()[0] != '0'
                    for line in processes.splitlines()), '请先停止普通用户的 Nginx 服务，再安装系统服务。')
    if 'nginx: master process' in processes:
        require(service_state('nginx')[0], '运行中的 Nginx 不属于本安装器的系统服务，请先停止其原服务再重试；配置文件会保留。')
    path = service_path('nginx')
    require(not path.is_symlink() and (not path.exists() or owner in path.read_text()),
            f'{path} 不归本安装器管理，拒绝覆盖。')
    atomic_write(path, service_definition('nginx', [shutil.which('nginx'), '-g', 'daemon off;'], owner), 0o644)


def init_path():
    os.environ['PATH'] = SERVICE_PATH + ':' + os.environ.get('PATH', '')
