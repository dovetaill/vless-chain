"""Enable system time synchronization without changing time zone or time sources."""
import os
import shutil
import subprocess
import sys
import time
from chain_common import require


TIME_UNITS = ('chrony.service', 'chronyd.service', 'systemd-timesyncd.service', 'ntp.service', 'ntpd.service', 'ntpsec.service')


def status():
    command = shutil.which('timedatectl')
    if not command:
        return {}
    result = subprocess.run([command, 'show', '--property=NTP', '--property=NTPSynchronized', '--property=Timezone'],
                            capture_output=True, text=True, timeout=10)
    return dict(line.split('=', 1) for line in result.stdout.splitlines() if '=' in line) if result.returncode == 0 else {}


def synchronize():
    try:
        _synchronize()
    except subprocess.TimeoutExpired as error:
        raise ValueError(f'时间同步步骤 {error.cmd[0]} 超时（{error.timeout} 秒）；请查看本次任务日志。') from None
    except subprocess.CalledProcessError as error:
        raise ValueError(f'时间同步步骤 {" ".join(error.cmd)} 失败（退出码 {error.returncode}）；请查看本次任务日志中的错误。') from None


def _synchronize():
    before = status()
    require(before, '无法查询系统时间状态；本操作需要可用的 systemd/timedatectl。')
    result = subprocess.run(['systemctl', 'list-unit-files', *TIME_UNITS, '--no-legend', '--no-pager'],
                            capture_output=True, text=True, timeout=10)
    # systemctl returns 1 with empty output when no requested unit files exist.
    if result.returncode != 0 and not (result.returncode == 1 and not result.stdout.strip() and not result.stderr.strip()):
        if result.stderr:
            print(result.stderr, file=sys.stderr, end='', flush=True)
        result.check_returncode()
    installed = result.stdout
    units = [unit for unit in TIME_UNITS if any(
        fields[0] == unit and fields[1] != 'alias'
        for fields in (line.split() for line in installed.splitlines()) if len(fields) >= 2)]
    active = [unit for unit in units if subprocess.run(['systemctl', 'is-active', '--quiet', unit], timeout=10).returncode == 0]
    require(len(active) <= 1, '检测到多个运行中的时间服务：' + '、'.join(active) + '；请先解决服务冲突。')
    if not units:
        if shutil.which('apt-get'):
            environment = dict(os.environ, DEBIAN_FRONTEND='noninteractive')
            subprocess.run(['apt-get', 'update'], check=True, timeout=300, env=environment)
            subprocess.run(['apt-get', 'install', '-y', 'chrony'], check=True, timeout=300, env=environment)
            units = ['chrony.service']
        elif shutil.which('dnf'):
            subprocess.run(['dnf', 'install', '-y', 'chrony'], check=True, timeout=300)
            units = ['chronyd.service']
        else:
            raise ValueError('未找到可用时间服务及支持的包管理器；请安装系统支持的 NTP 服务后重试。')
    unit = (active or units)[0]
    print(f'启用时间服务 {unit}；保留现有时间源和时区。', flush=True)
    subprocess.run(['systemctl', 'enable', '--now', unit], check=True, timeout=30)
    if unit in ('chrony.service', 'chronyd.service'):
        require(shutil.which('chronyc'), f'{unit} 已存在，但缺少 chronyc；请修复 chrony 安装。')
        source = subprocess.run(['chronyc', 'waitsync', '30', '0', '0', '2'], timeout=70)
        require(source.returncode == 0, 'chrony 尚未取得有效同步时间源；请检查 NTP 网络及 chronyc sources -v。')
        print('已确认有效时间源；立即校准系统时间，可能产生时间跳变。', flush=True)
        subprocess.run(['chronyc', 'makestep'], check=True, timeout=10)
        result = subprocess.run(['chronyc', 'waitsync', '30', '0.1', '0', '2'], timeout=70)
        require(result.returncode == 0, 'chrony 尚未确认同步或剩余时间偏差超过 0.1 秒；请检查 NTP 网络及 chronyc sources -v、chronyc tracking。')
        after = status()
        require(after.get('Timezone') == before.get('Timezone'), '时区发生并发变化，请检查；本操作没有修改时区。')
    else:
        for _ in range(30):
            after = status()
            require(after.get('Timezone') == before.get('Timezone'), '时区发生并发变化，请检查；本操作没有修改时区。')
            if after.get('NTPSynchronized') == 'yes':
                break
            time.sleep(2)
        else:
            raise ValueError(f'{unit} 已启用，但 60 秒内尚未确认同步；请检查 NTP 网络或 journalctl -u {unit}，不代表同步成功。')
    print('时间同步已启用且确认同步；时区保持 ' + before.get('Timezone', '未知'))


if __name__ == '__main__':
    try:
        synchronize()
    except (ValueError, OSError, subprocess.SubprocessError) as error:
        print(f'时间同步失败：{error}', file=sys.stderr)
        sys.exit(1)
