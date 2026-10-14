#!/usr/bin/env python3
"""Exercise controller-owned local enrollment without touching system services."""
import os
from pathlib import Path
import signal
import tempfile
import urllib.request

from smoke import Harness


def children(pid):
    result = []
    for entry in Path('/proc').iterdir():
        if not entry.name.isdigit():
            continue
        try:
            fields = (entry / 'stat').read_text().rsplit(')', 1)[1].split()
            if int(fields[1]) == pid:
                result.append(int(entry.name))
        except (FileNotFoundError, ProcessLookupError):
            pass
    return result


def exited(pid):
    try:
        return Path(f'/proc/{pid}/stat').read_text().rsplit(')', 1)[1].split()[0] == 'Z'
    except FileNotFoundError:
        return True


def main():
    with tempfile.TemporaryDirectory(prefix='chain-local-web-', dir=os.environ['CHAIN_TEST_ROOT']) as temporary:
        root = Path(temporary)
        harness = Harness(root)
        try:
            port = harness.port()
            host = os.environ.get('CHAIN_TEST_LOCAL_HOST', '127.0.0.1')
            harness.base = f'http://{host}:{port}'
            harness.client = urllib.request.build_opener(urllib.request.ProxyHandler({}),
                urllib.request.HTTPCookieProcessor(harness.cookie_jar))
            data = root / 'panel'
            env = {'CHAIN_DATA_DIR': str(data), 'CHAIN_LISTEN': f'{host}:{port}',
                   'NO_PROXY': host, 'no_proxy': host,
                   'CHAIN_XRAY_BIN': str(harness.xray)}
            panel = harness.launch('panel', harness.bin_dir / 'chain-panel', env)
            harness.wait(lambda: (data / 'admin-token').exists(), 'panel credentials')
            harness.wait(lambda: harness.request('/') or True, 'panel listener')
            harness.request('/api/login', 'POST', {'token': (data / 'admin-token').read_text()})
            assert harness.state()['local_node_id'] is None
            node = harness.request('/api/local-node', 'POST', {'host': '127.0.0.1'})['id']
            harness.wait(lambda: any(n['id'] == node and n['online'] for n in harness.state()['nodes']), 'local Agent online')
            assert harness.request('/api/local-node', 'POST', {'host': '127.0.0.1'})['id'] == node
            assert len(harness.state()['nodes']) == 1
            assert harness.state()['local_node_id'] == node
            harness.request(f'/api/nodes/{node}', 'DELETE', status=409)
            harness.request(f'/api/nodes/{node}/token', 'POST', {}, status=409)
            harness.request('/api/lines', 'POST', {'name': 'local SS', 'kind': 'vless',
                'hops': [{'node_id': node, 'port': harness.port()}],
                'access': {'protocol': 'shadowsocks'}})
            harness.publish()
            owned = children(panel.pid)
            assert owned, 'controller must own the local Agent'
            descendants = owned + [child for pid in owned for child in children(pid)]
            seen = next(n['seen'] for n in harness.state()['nodes'] if n['id'] == node)
            os.kill(panel.pid, signal.SIGKILL)
            panel.wait(timeout=10)
            harness.wait(lambda: all(exited(pid) for pid in descendants), 'abrupt controller exit cleans Agent and Xray')
            panel = harness.launch('panel-restarted', harness.bin_dir / 'chain-panel', env)
            harness.wait(lambda: harness.request('/') or True, 'restarted panel')
            harness.request('/api/login', 'POST', {'token': (data / 'admin-token').read_text()})
            harness.wait(lambda: any(n['id'] == node and n['online'] and n['seen'] > seen
                                    for n in harness.state()['nodes']), 'local Agent resumed')
            assert harness.state()['local_node_id'] == node
            print('PASS web local enrollment, identity, repeat creation, protected credentials, and restart.', flush=True)
        finally:
            harness.close()
    assert not root.exists()


if __name__ == '__main__':
    signal.signal(signal.SIGTERM, lambda *_: (_ for _ in ()).throw(KeyboardInterrupt()))
    main()
