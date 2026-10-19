"""Explicit operator action only; never change the configured time zone."""
import shutil
import subprocess
import time
from chain_common import require


def status():
    command = shutil.which('timedatectl')
    if not command:
        return {}
    result = subprocess.run([command, 'show', '--property=NTP', '--property=NTPSynchronized', '--property=Timezone'],
                            capture_output=True, text=True, timeout=10)
    return dict(line.split('=', 1) for line in result.stdout.splitlines() if '=' in line) if result.returncode == 0 else {}


def synchronize():
    before = status()
    require(before, '本操作需要 systemd/timedatectl；请在该系统的时间服务中开启同步。')
    installed = subprocess.run(['systemctl', 'list-unit-files', 'systemd-timesyncd.service', 'chronyd.service', 'chrony.service', 'ntp.service', '--no-legend'],
                               capture_output=True, text=True, check=True, timeout=10).stdout
    if not any(line.split()[0] in ('systemd-timesyncd.service', 'chronyd.service', 'chrony.service', 'ntp.service') for line in installed.splitlines() if line.split()):
        if shutil.which('apt-get'):
            subprocess.run(['apt-get', 'update'], check=True, timeout=300)
            subprocess.run(['apt-get', 'install', '-y', 'systemd-timesyncd'], check=True, timeout=300)
        elif shutil.which('dnf'):
            subprocess.run(['dnf', 'install', '-y', 'chrony'], check=True, timeout=300)
        else:
            raise ValueError('未找到可用时间服务；请安装系统支持的 NTP 服务后重试。')
    subprocess.run(['timedatectl', 'set-ntp', 'true'], check=True, timeout=30)
    for _ in range(30):
        after = status()
        require(after.get('Timezone') == before.get('Timezone'), '时区发生并发变化，请检查；本操作没有修改时区。')
        if after.get('NTPSynchronized') == 'yes':
            print('时间同步已启用且确认同步；时区保持 ' + before.get('Timezone', '未知'))
            return
        time.sleep(2)
    raise ValueError('已启用自动同步，但 60 秒内尚未确认同步。请检查 NTP 网络或时间服务日志，不代表同步成功。')


if __name__ == '__main__':
    synchronize()
