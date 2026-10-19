"""Browser diagnostics against isolated real panel/Agent; never modifies system time."""
import gzip
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import time

from smoke import Harness


def exercise(root):
    h = Harness(root)
    try:
        port = h.port()
        h.base = f'http://127.0.0.1:{port}'
        h.launch('control', h.bin_dir / 'chain-panel', {
            'CHAIN_DATA_DIR': str(root / 'control'), 'CHAIN_LISTEN': f'127.0.0.1:{port}',
            'CHAIN_XRAY_BIN': str(h.xray)})
        h.wait(lambda: h.request('/') or True, 'panel start')
        h.login()
        node = h.request('/api/nodes', 'POST', {'name': '浏览器测试节点', 'host': '127.0.0.1'})
        token = root / 'agent-token'
        token.write_text(node['token'])
        token.chmod(0o600)
        agent_dir = root / 'agent'
        h.launch('agent', h.bin_dir / 'chain-agent', {
            'CHAIN_AGENT_DIR': str(agent_dir), 'CHAIN_PANEL_URL': h.base,
            'CHAIN_NODE_ID': node['id'], 'CHAIN_NODE_TOKEN_FILE': str(token),
            'CHAIN_XRAY_BIN': str(h.xray)})
        h.wait(lambda: any((n['report'].get('system') or {}).get('diagnostics_version') == 1 for n in h.state()['nodes']), 'diagnostics capability')
        line = h.request('/api/lines', 'POST', {'name': '浏览器诊断代理', 'kind': 'vless',
                         'hops': [{'node_id': node['id'], 'port': h.port()}]})
        tag = f"{int(time.time())} line-{line['id']}-in "
        # Owned fixture files model core output; the read travels through the real Agent broker.
        (agent_dir / 'xray.log').write_text(tag + 'fixture-live\n' + tag + 'password=fixture-password\n')
        with gzip.open(agent_dir / 'xray.log.123.gz', 'wt') as archive:
            archive.write(tag + 'fixture-archive\n')
        browser = h.launch('browser', Path(shutil.which('node')), {}, args=[
            str(Path(__file__).with_name('diagnostics-ui.cjs')), h.base,
            str(root / 'control/admin-token'), str(root), str(agent_dir), line['id']])
        assert browser.wait(timeout=180) == 0, (root / 'browser.log').read_text()
        print((root / 'browser.log').read_text(), flush=True)
        # Keep screenshots only when the caller explicitly owns their output directory.
        if os.environ.get('CHAIN_TEST_SCREENSHOTS'):
            for image in root.glob('*.png'):
                shutil.copyfile(image, Path(os.environ['CHAIN_TEST_SCREENSHOTS']) / image.name)
    finally:
        h.close()


if __name__ == '__main__':
    with tempfile.TemporaryDirectory(prefix='diagnostics-browser-', dir=os.environ.get('CHAIN_TEST_ROOT')) as directory:
        exercise(Path(directory))
