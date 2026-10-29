"""One-time bridge for old installers that copy a fixed list of Python filenames.

New installations execute bin/chainctl directly. No installation or maintenance
logic lives here; remove this compatibility bridge when pre-native upgrades end.
"""
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys


def main():
    args = sys.argv[1:]
    root = Path(args[args.index('--root') + 1] if '--root' in args else '/').resolve(strict=True)
    base = root / 'opt/chain'
    state = json.loads((base / '.chain-install.json').read_text())
    if state.get('owner') != 'chain-panel-installer-v1':
        raise ValueError('安装目录归属不匹配')
    binary = None
    for role in ('panel', 'agent'):
        record = state.get('roles', {}).get(role)
        if not record:
            continue
        release = (base / ('current-' + role)).resolve(strict=True)
        if release.parent != (base / 'releases').resolve(strict=True) or release.name != record['current']:
            raise ValueError('当前程序链接与安装状态不一致')
        candidate = release / 'bin/chainctl'
        if candidate.is_file() and not candidate.is_symlink():
            manifest = json.loads((release / 'release.json').read_text())
            expected = manifest['files']['bin/chainctl']['sha256']
            with candidate.open('rb') as stream:
                digest = hashlib.sha256()
                for data in iter(lambda: stream.read(1024 * 1024), b''):
                    digest.update(data)
            if digest.hexdigest() != expected:
                raise ValueError('Rust chainctl 校验失败')
            binary = candidate
            break
    if binary is None:
        raise ValueError('发行包缺少 Rust chainctl，请通过新版在线安装脚本更新')
    name = Path(__file__).name
    if name == 'chain_maintenance.py':
        args = ['maintenance']
    elif name == 'chain_ingress.py':
        action = args.pop(0) if args else 'apply'
        args.insert(0, {'enable': 'ingress-enable', 'disable': 'ingress-disable', 'apply': 'internal-ingress-apply', 'watch': 'internal-ingress-watch'}[action])
    subprocess.run([str(binary), 'internal-adopt', '--root', str(root)], check=True)
    os.execv(binary, [str(binary), *args])


if __name__ == '__main__':
    main()
