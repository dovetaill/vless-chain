#!/usr/bin/env python3
"""Build and exercise the panel with task-owned caches, processes and listeners."""
import argparse
import contextlib
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import tempfile
import time


def owned_processes(marker):
    result = []
    for directory in Path('/proc').iterdir():
        if not directory.name.isdigit():
            continue
        try:
            if marker in (directory / 'environ').read_bytes().split(b'\0'):
                result.append(int(directory.name))
        except (OSError, ProcessLookupError):
            continue
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--xray', required=True, type=Path)
    parser.add_argument('--work-parent', required=True, type=Path, help='Spacious disk, not a small tmpfs')
    parser.add_argument('--browser', action='store_true')
    args = parser.parse_args()
    xray = args.xray.resolve(strict=True)
    project = Path(__file__).resolve().parents[1]
    root = Path(tempfile.mkdtemp(prefix='.chain-verify-', dir=args.work_parent.resolve(strict=True)))
    before = shutil.disk_usage(root)
    marker = f'CHAIN_VERIFY_OWNER={root}'.encode()
    env = os.environ.copy()
    env.update(CHAIN_VERIFY_OWNER=str(root), CARGO_TARGET_DIR=str(root / 'target'), CARGO_INCREMENTAL='0',
               CARGO_PROFILE_DEV_DEBUG='0', CARGO_PROFILE_TEST_DEBUG='0', PYTHONDONTWRITEBYTECODE='1',
               npm_config_cache=str(root / 'npm-cache'), npm_config_progress='false', CI='1',
               PLAYWRIGHT_BROWSERS_PATH=str(root / 'browsers'), TMPDIR=str(root / 'tmp'),
               CHAIN_WEB_ASSETS=str(root / 'frontend' / 'dist'), CHAIN_TEST_ROOT=str(root),
               CHAIN_TEST_BIN_DIR=str(root / 'target' / 'debug'), CHAIN_TEST_XRAY=str(xray))
    (root / 'tmp').mkdir()
    commands = []
    manifest = {'pid': os.getpid(), 'pgid': os.getpgrp(), 'cwd': str(project), 'root': str(root), 'groups': []}

    def run(command, cwd=project):
        proc = subprocess.Popen(command, cwd=cwd, env=env, start_new_session=True)
        commands.append(proc)
        manifest['groups'].append(proc.pid)
        (root / 'manifest.json').write_text(json.dumps(manifest))
        if proc.wait():
            raise RuntimeError(f'Command failed: {command}')

    def interrupted(*_):
        raise KeyboardInterrupt()

    signal.signal(signal.SIGTERM, interrupted)
    signal.signal(signal.SIGINT, interrupted)
    print('Verification workspace: ' + str(root), flush=True)
    try:
        shutil.copytree(project / 'frontend', root / 'frontend', ignore=shutil.ignore_patterns('node_modules', 'dist'))
        run(['npm', 'ci', '--no-audit', '--no-fund'], root / 'frontend')
        run(['npm', 'run', 'build'], root / 'frontend')
        run(['cargo', 'fmt', '--check'])
        run(['cargo', 'test', '--locked', '--quiet'])
        run(['cargo', 'build', '--locked', '--bins', '--quiet'])
        if args.browser:
            directory = root / 'browser-tools'
            directory.mkdir()
            (directory / 'package.json').write_text('{"private":true,"dependencies":{"playwright":"1.64.0"}}')
            run(['npm', 'install', '--no-audit', '--no-fund'], directory)
            run(['node', 'node_modules/playwright/cli.js', 'install', 'chromium'], directory)
            env['CHAIN_TEST_BROWSER'] = str(directory / 'node_modules' / 'playwright')
            run(['node', 'tests/resource-flow.cjs', str(root / 'frontend' / 'dist')])
        run(['python3', 'tests/smoke.py'])
        run(['python3', '-B', 'tests/local_web.py'])
        run(['python3', '-B', 'tests/proxy_flow.py'])
        print('Checks passed; verifying cleanup.', flush=True)
    finally:
        # Stop only children carrying this invocation's ownership marker.
        groups = set()
        for pid in owned_processes(marker):
            with contextlib.suppress(ProcessLookupError):
                groups.add(os.getpgid(pid))
        groups.discard(os.getpgrp())
        for group in groups:
            with contextlib.suppress(ProcessLookupError):
                os.killpg(group, signal.SIGTERM)
        for proc in commands:
            with contextlib.suppress(subprocess.TimeoutExpired):
                proc.wait(timeout=6)
        deadline = time.monotonic() + 8
        while owned_processes(marker) and time.monotonic() < deadline:
            time.sleep(.1)
        for pid in owned_processes(marker):
            with contextlib.suppress(ProcessLookupError):
                os.kill(pid, signal.SIGKILL)
        for proc in commands:
            with contextlib.suppress(subprocess.TimeoutExpired):
                proc.wait(timeout=3)
        time.sleep(.2)
        remaining = owned_processes(marker)
        if remaining:
            raise RuntimeError(f'Cleanup incomplete; PIDs={remaining}, artifacts preserved at {root}')
        # Detect files still held by other processes before deleting artifacts.
        open_files = []
        for directory in Path('/proc').iterdir():
            if not directory.name.isdigit():
                continue
            with contextlib.suppress(OSError):
                for link in (directory / 'fd').iterdir():
                    with contextlib.suppress(OSError):
                        if str(link.readlink()).startswith(str(root) + '/'):
                            open_files.append(f'{directory.name}:{link.name}')
        if open_files:
            raise RuntimeError(f'Open artifacts remain: {open_files}; preserved at {root}')
        size = sum(p.stat().st_size for p in root.rglob('*') if p.is_file())
        shutil.rmtree(root)
        assert not root.exists()
        after = shutil.disk_usage(args.work_parent)
        print(f'CLEANUP groups={sorted(groups)}; no owned processes or open files; removed={size} bytes; '
              f'disk_free_before={before.free}; disk_free_after={after.free}; workspace_removed={root}', flush=True)


if __name__ == '__main__':
    main()
