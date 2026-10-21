"""Socket-activated, fixed-operation maintenance executor; never accepts shell text."""
import concurrent.futures
import json
import os
from pathlib import Path
import pwd
import re
import signal
import shutil
import socket
import struct
import subprocess
import sys
import tempfile
import threading
import time
import uuid

from chain_common import VERSION_RE, download_release, json_write, require, safe_extract
from chain_network import SITE_OWNER

STATE = Path('/var/lib/chain-maintenance')
INSTALL_STATE = Path('/opt/chain/.chain-install.json')
LOCK = threading.Lock()
RELOAD = threading.Event()
ACTIONS = {'upgrade', 'restart-all', 'restart-panel', 'restart-agent', 'sync-time'}
HTTPS_UNIT = Path('/etc/systemd/system/chain-panel-https.service')


def validate(value):
    require(isinstance(value, dict) and set(value) <= {'id', 'action', 'version', 'xray_channel'}, '请求字段无效')
    require(isinstance(value.get('id'), str) and str(uuid.UUID(value['id'])) == value['id'], '任务 ID 无效')
    require(value.get('action') in ACTIONS | {'status'}, '不支持的维护操作')
    if value['action'] == 'upgrade':
        require(isinstance(value.get('version'), str) and re.fullmatch(VERSION_RE, value['version']), '请选择正式发行版本')
        require(value.get('xray_channel', 'recommended') in ('recommended', 'latest'), 'Xray 来源无效')
    else:
        require('xray_channel' not in value, '只有升级操作可选择内核来源')
    return value


def installed():
    state = json.loads(INSTALL_STATE.read_text())
    require(state.get('owner') == 'chain-panel-installer-v1', '安装归属不匹配')
    return {role: data.get('version') for role, data in state['roles'].items()}


def related_services(roles):
    services = [f'chain-{role}.service' for role in ('panel', 'agent') if role in roles]
    require(services, '未安装面板或 Agent')
    if HTTPS_UNIT.exists() or HTTPS_UNIT.is_symlink():
        require(not HTTPS_UNIT.is_symlink() and HTTPS_UNIT.is_file()
                and HTTPS_UNIT.read_text().startswith(SITE_OWNER + '\n'), '独立 HTTPS 服务归属不匹配')
        services.append(HTTPS_UNIT.name)
    return services


def time_output(path):
    with path.open('rb') as stream:
        size = stream.seek(0, os.SEEK_END)
        stream.seek(max(0, size - 65536))
        if size > 65536:
            if not stream.readline(65536).endswith(b'\n'):
                return ''  # Never retain a partial line that could begin inside a credential.
        output = stream.read(65536).decode('utf-8', errors='replace')
    output = re.sub(r'\x1b\[[0-?]*[ -/]*[@-~]', '', output)
    output = re.sub(r'[\x00-\x08\x0b-\x1f\x7f]', '', output)
    output = re.sub(r'(?i)\b[a-z][a-z0-9+.-]*://\S+', '[地址已隐藏]', output)
    output = re.sub(r'(?im)\b(?:authorization|proxy-authorization)\s*[:=].*', '[认证信息已隐藏]', output)
    output = re.sub(r'(?im)^.*\b(?:[\w-]*(?:token|password|passwd|secret|api[_-]?key))\b[\"\']?\s*[:=].*$',
                    '[凭据已隐藏]', output)
    return output.strip()[-4096:]


def execute(job, directory=STATE):
    path = directory / (job['id'] + '.json')
    process = None
    try:
        before = installed()
        services = related_services(before) if job['action'] in ('upgrade', 'restart-all') else []
        job.update(state='running', before=before, started=int(time.time()))
        json_write(path, job)
        with tempfile.TemporaryDirectory(prefix='work-' + job['id'] + '-', dir=directory) as temporary:
            temporary = Path(temporary)
            if job['action'] == 'upgrade':
                current = before.get('panel')
                require(current is not None, '未安装面板')
                require(all(tuple(map(int, job['version'].split('.'))) >= tuple(map(int, before[role].split('.')))
                            for role in ('panel', 'agent') if role in before),
                        '不允许自动降级组件或数据库；请人工核对兼容性')
                archive, _ = download_release(temporary, job['version'])
                manifest = safe_extract(archive, temporary / 'inspect')
                require(manifest['version'] == job['version'] and 'scripts/chain_maintenance.py' in manifest['files'],
                        '目标版本不支持远程维护，拒绝升级以免丢失控制能力')
                commands = [['/usr/local/bin/chainctl', 'update', '--component', 'panel', '--package', str(archive), '--non-interactive']]
                if job.get('xray_channel') == 'latest':
                    commands[0].extend(['--xray-channel', 'latest'])
                if HTTPS_UNIT.name in services:
                    commands.append(['/usr/bin/systemctl', 'restart', HTTPS_UNIT.name])
            elif job['action'] == 'restart-all':
                commands = [['/usr/bin/systemctl', 'restart', *services]]
            elif job['action'] == 'sync-time':
                commands = [[sys.executable, '-B', str(Path(__file__).with_name('chain_time.py'))]]
            else:
                role = job['action'].removeprefix('restart-')
                require(role in before, '该组件未安装')
                commands = [['/usr/bin/systemctl', 'restart', 'chain-' + role + '.service']]
                services = ['chain-' + role + '.service']
            with (temporary / 'operation.log').open('wb') as log:
                for command in commands:
                    process = subprocess.Popen(command, stdout=log, stderr=subprocess.STDOUT, start_new_session=True,
                                               env={**os.environ, 'PYTHONDONTWRITEBYTECODE': '1'})
                    job['pid'] = process.pid
                    json_write(path, job)
                    try:
                        result = process.wait(timeout=1800)
                    except subprocess.TimeoutExpired:
                        os.killpg(process.pid, signal.SIGTERM)
                        try:
                            process.wait(timeout=10)
                        except subprocess.TimeoutExpired:
                            os.killpg(process.pid, signal.SIGKILL)
                            process.wait()
                        raise ValueError('维护操作超时；已停止本任务进程，请检查逐组件状态') from None
                    finally:
                        try:
                            os.killpg(process.pid, signal.SIGTERM)
                        except ProcessLookupError:
                            pass
                        if job['action'] == 'sync-time':
                            log.flush()
                            job['detail'] = time_output(temporary / 'operation.log')
                    require(result == 0, '维护操作失败；请在该 VPS 检查安装状态与服务日志')
            job['after'] = installed()
            for service in services:
                require(subprocess.run(['/usr/bin/systemctl', 'is-active', '--quiet', service], timeout=30).returncode == 0,
                        f'{service} 服务未运行')
            if job['action'] == 'upgrade':
                for role in ('panel', 'agent'):
                    if role in before:
                        require(job['after'].get(role) == job['version'], f'{role} 版本未达到目标')
            job.update(state='succeeded', detail=(job.get('detail') or '时间同步已确认，时区保持不变')
                       if job['action'] == 'sync-time' else '组件检查通过；总控还需检查整链')
    except Exception as error:
        message = str(error)
        if job['action'] == 'sync-time' and job.get('detail'):
            message += '\n' + job['detail']
        job.update(state='failed', error=message)
        try:
            job['after'] = installed()
        except Exception:
            pass
    finally:
        if process is not None:
            try:
                os.killpg(process.pid, signal.SIGTERM)
            except ProcessLookupError:
                pass
            try:
                process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                os.killpg(process.pid, signal.SIGKILL)
                process.wait()
        job.pop('pid', None)
        job['finished'] = int(time.time())
        json_write(path, job)
        if job['action'] == 'upgrade':
            RELOAD.set()


def submit(value, pool, directory=STATE):
    value = validate(value)
    path = directory / (value['id'] + '.json')
    with LOCK:
        if path.exists():
            job = json.loads(path.read_text())
            require(value['action'] == 'status' or (job['action'], job.get('version', ''), job.get('xray_channel', 'recommended')) ==
                    (value['action'], value.get('version', ''), value.get('xray_channel', 'recommended')), '任务 ID 已用于其他操作')
            return job
        if value['action'] == 'status':
            return {'id': value['id'], 'state': 'not_found'}
        jobs = list(directory.glob('*.json'))
        require(not any(json.loads(p.read_text()).get('state') in ('queued', 'running') for p in jobs), '已有维护任务正在执行')
        job = {**value, 'state': 'queued', 'created': int(time.time())}
        json_write(path, job)
        pool.submit(execute, job.copy(), directory)
        # Bounded history; current jobs and recent idempotency receipts are retained.
        for old in sorted(jobs, key=lambda p: p.stat().st_mtime)[:-200]:
            if time.time() - old.stat().st_mtime > 30 * 86400:
                old.unlink()
        return job


def main():
    require(os.geteuid() == 0, '维护组件须由 systemd 启动')
    require(os.environ.get('LISTEN_PID') == str(os.getpid()) and os.environ.get('LISTEN_FDS') == '1', '需要 systemd socket activation')
    panel_uid = pwd.getpwnam('chain-panel').pw_uid
    listener = socket.socket(fileno=3)
    listener.settimeout(1)
    # Do not replay interrupted upgrades: their externally visible result is unknown.
    for path in STATE.glob('*.json'):
        job = json.loads(path.read_text())
        if job.get('state') in ('queued', 'running'):
            job.update(state='unknown', error='执行组件中断，请检查当前版本后重新创建任务')
            json_write(path, job)
        if path.stem == str(uuid.UUID(job['id'])):
            for temporary in STATE.glob('work-' + job['id'] + '-*'):
                require(temporary.is_dir() and not temporary.is_symlink() and temporary.stat().st_uid == 0,
                        '维护临时目录归属不匹配，未清理')
                shutil.rmtree(temporary)
    with concurrent.futures.ThreadPoolExecutor(max_workers=1) as pool:
        while True:
            if RELOAD.is_set():
                break
            try:
                connection, _ = listener.accept()
            except socket.timeout:
                continue
            with connection:
                try:
                    _, uid, _ = struct.unpack('3i', connection.getsockopt(socket.SOL_SOCKET, socket.SO_PEERCRED, 12))
                    require(uid in (0, panel_uid), '未授权的本机进程')
                    connection.settimeout(3)
                    with connection.makefile('rb') as incoming:
                        line = incoming.readline(8193)
                    require(len(line) <= 8192 and line.endswith(b'\n'), '请求过大或不完整')
                    result = submit(json.loads(line), pool)
                except Exception as error:
                    result = {'error': str(error)}
                try:
                    connection.sendall(json.dumps(result).encode() + b'\n')
                except OSError:
                    pass


if __name__ == '__main__':
    main()
