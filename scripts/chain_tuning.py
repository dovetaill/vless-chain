"""Set TCP defaults and switch default interface queues without restarting services."""
import json
import os
from pathlib import Path
import re
import shutil
import stat
import subprocess
import sys

OWNER = '# chain-panel network defaults v1\n'
CONFIG = Path('/etc/sysctl.d/99-chain-network.conf')
MODULE_CONFIG = Path('/etc/modules-load.d/chain-network.conf')
KEYS = ('net.ipv4.tcp_congestion_control', 'net.core.default_qdisc')
QUEUES = ('fq', 'fq_codel', 'fq_pie', 'cake', 'pfifo_fast')


def validate(cc, qdisc):
    if not isinstance(cc, str) or not re.fullmatch(r'[a-z][a-z0-9_]{0,31}', cc) or qdisc not in QUEUES:
        raise ValueError('拥塞算法或队列无效')


def read_value(key):
    return (Path('/proc/sys') / key.replace('.', '/')).read_text().strip()


def command(*args):
    result = subprocess.run(args, capture_output=True, text=True, timeout=3 if args[0] == 'tc' else 15)
    if result.returncode:
        raise ValueError(f'{args[0]} 执行失败：{result.stderr.strip()[:512]}')
    return result.stdout


def modules():
    directory = Path('/lib/modules') / os.uname().release
    names = set()
    for filename in ('modules.builtin', 'modules.dep'):
        try:
            content = (directory / filename).read_text()
        except FileNotFoundError:
            continue
        names.update(re.findall(r'/((?:tcp|sch)_[a-z0-9_]+)\.ko', content))
    names.update(p.name for p in Path('/sys/module').glob('*') if p.name.startswith(('tcp_', 'sch_')))
    return names


def status():
    available = read_value('net.ipv4.tcp_available_congestion_control').split()
    cc, qdisc = (read_value(key) for key in KEYS)
    present = modules()
    algorithms = sorted(set(available + [cc] + [name for name in ('bbr', 'bbrplus', 'cubic') if f'tcp_{name}' in present]))
    queues = [name for name in QUEUES if name == qdisc or f'sch_{name}' in present]
    actual, warning = [], ''
    try:
        actual = json.loads(command('tc', '-j', 'qdisc', 'show'))
        queues = [name for name in QUEUES if name in queues or any(q.get('kind') == name for q in actual)]
    except (OSError, ValueError, subprocess.TimeoutExpired) as error:
        warning = f'未能读取网卡实际队列：{error}'
    return {'congestion_control': cc, 'qdisc': qdisc, 'algorithms': algorithms, 'queues': queues,
            'interfaces': [{'dev': q.get('dev'), 'kind': q.get('kind'), 'parent': q.get('parent', 'root')} for q in actual],
            'warning': warning, 'kernel': os.uname().release}


def owned_content(path):
    try:
        info = path.lstat()
    except FileNotFoundError:
        return None
    if not stat.S_ISREG(info.st_mode) or info.st_uid != os.geteuid() or info.st_nlink != 1 or info.st_mode & 0o022:
        raise ValueError(f'{path} 类型、归属或权限不匹配，未覆盖')
    content = path.read_bytes()
    if not content.startswith(OWNER.encode()):
        raise ValueError(f'{path} 不属于本项目，未覆盖')
    return content


def replace_owned(path, expected, content):
    # Keep existing ownership, mode, ACLs and xattrs; reject concurrent content changes.
    if owned_content(path) != expected:
        raise ValueError(f'{path} 已被其他操作修改，未覆盖')
    if content is None:
        path.unlink()
        return
    flags = os.O_RDWR | os.O_NOFOLLOW
    if expected is None:
        flags |= os.O_CREAT | os.O_EXCL
    fd = os.open(path, flags, 0o644)
    with os.fdopen(fd, 'r+b') as stream:
        info = os.fstat(stream.fileno())
        if not stat.S_ISREG(info.st_mode) or info.st_uid != os.geteuid() or info.st_nlink != 1:
            raise ValueError(f'{path} 归属变化，未写入')
        if stream.read() != (expected or b''):
            raise ValueError(f'{path} 已被其他操作修改，未写入')
        try:
            stream.seek(0)
            stream.write(content)
            stream.truncate()
            stream.flush()
            os.fsync(stream.fileno())
        except Exception:
            stream.seek(0)
            stream.write(expected or b'')
            stream.truncate()
            stream.flush()
            os.fsync(stream.fileno())
            if expected is None:
                path.unlink()
            raise


def apply_queues(qdisc):
    warnings = []
    try:
        queues = json.loads(command('tc', '-j', 'qdisc', 'show'))
    except (OSError, ValueError, subprocess.TimeoutExpired) as error:
        return [f'默认设置已保存，网卡队列未切换：{error}']
    for queue in queues:
        kind, dev = queue.get('kind'), queue.get('dev')
        if kind in ('noqueue', 'mq', 'ingress', 'clsact') or kind == qdisc:
            continue
        parent = queue.get('parent', 'root')
        label = f'{dev} / {parent}'
        # shortcut: 仅替换内核自动创建的 0: 队列；支持自定义队列前需完整保存其配置及过滤规则。
        root = next((q for q in queues if q.get('dev') == dev and q.get('root')), None)
        if (kind not in ('fq', 'fq_codel', 'fq_pie', 'pfifo_fast') or queue.get('handle') != '0:'
                or (not queue.get('root') and (not root or root.get('kind') != 'mq'))):
            warnings.append(f'{label} 保留现有 {kind} 队列，未覆盖自定义配置')
            continue
        try:
            if json.loads(command('tc', '-j', 'filter', 'show', 'dev', dev)):
                warnings.append(f'{label} 存在流量过滤规则，未切换队列')
                continue
            current = json.loads(command('tc', '-j', 'qdisc', 'show', 'dev', dev))
            if queue not in current:
                warnings.append(f'{label} 队列已被其他操作修改，未覆盖')
                continue
            location = ('root',) if queue.get('root') else ('parent', parent)
            command('tc', 'qdisc', 'replace', 'dev', dev, *location, qdisc)
            current = json.loads(command('tc', '-j', 'qdisc', 'show', 'dev', dev))
            if not any(q.get('parent', 'root') == parent and q.get('kind') == qdisc for q in current):
                warnings.append(f'{label} 切换后未确认 {qdisc} 生效，请核对实际队列')
        except (OSError, ValueError, subprocess.TimeoutExpired) as error:
            warnings.append(f'{label} 队列切换未完成：{error}')
    return warnings


def apply(cc, qdisc):
    validate(cc, qdisc)
    before = status()
    if cc not in before['algorithms'] or qdisc not in before['queues']:
        raise ValueError('当前内核未检测到所选算法或队列；未安装任何模块或内核')
    if not shutil.which('sysctl'):
        raise ValueError('系统缺少 sysctl，未修改配置')
    paths = (CONFIG, MODULE_CONFIG)
    previous = [owned_content(path) for path in paths]
    present = modules()
    required = [name for name in (f'tcp_{cc}', f'sch_{qdisc}') if name in present]
    loaded = []
    desired = (cc, qdisc)
    original = (before['congestion_control'], before['qdisc'])
    content = [(OWNER + ''.join(f'{key} = {value}\n' for key, value in zip(KEYS, desired))).encode(),
               (OWNER + ''.join(f'{name}\n' for name in required)).encode()]
    written, applied = [], []
    try:
        for module in required:
            if not (Path('/sys/module') / module).exists():
                command('modprobe', module)
                loaded.append(module)
        for index, (key, value) in enumerate(zip(KEYS, desired)):
            applied.append(index)
            command('sysctl', '-w', f'{key}={value}')
            if read_value(key) != value:
                raise ValueError(f'{key} 未生效')
        for index, path in enumerate(paths):
            replace_owned(path, previous[index], content[index])
            written.append(index)
        after = status()
        if (after['congestion_control'], after['qdisc']) != desired:
            raise ValueError('设置被其他程序修改，请检查系统网络配置')
    except Exception as error:
        rollback_errors = []
        for index in reversed(written):
            try:
                replace_owned(paths[index], content[index], previous[index])
            except Exception as failure:
                rollback_errors.append(str(failure))
        for index in reversed(applied):
            try:
                current = read_value(KEYS[index])
                if current == original[index]:
                    continue
                if current != desired[index]:
                    raise ValueError(f'{KEYS[index]} 已被其他操作修改，未覆盖')
                command('sysctl', '-w', f'{KEYS[index]}={original[index]}')
                if read_value(KEYS[index]) != original[index]:
                    raise ValueError(f'{KEYS[index]} 恢复校验失败')
            except Exception as failure:
                rollback_errors.append(str(failure))
        detail = '；恢复未完成：' + '；'.join(rollback_errors) if rollback_errors else '；已恢复本次写入的默认参数和配置'
        if loaded:
            detail += '；本次加载的模块仍在内核中：' + ', '.join(loaded)
        raise ValueError(str(error) + detail) from error
    # Commit defaults first; queue failures are reported per interface, never as a fictitious rollback.
    warnings = apply_queues(qdisc)
    after = status()
    after['warning'] = '；'.join(filter(None, [after['warning'], *warnings]))
    return {'before': before, 'after': after, 'loaded_modules': loaded,
            'detail': 'TCP 默认算法及默认队列已保存，已尝试在线应用网卡队列，请核对实际队列及跳过项。'
                      '无需重启服务器或服务；已有连接和监听器不会自动切换 TCP 算法。'}


if __name__ == '__main__':
    try:
        if len(sys.argv) == 2 and sys.argv[1] == 'status':
            result = status()
        elif len(sys.argv) == 3 and os.geteuid() == 0:
            result = apply(*sys.argv[1:])
        else:
            raise ValueError('网络设置参数或执行身份无效')
        print(json.dumps(result, ensure_ascii=False))
    except Exception as error:
        print(str(error), file=sys.stderr)
        sys.exit(1)
