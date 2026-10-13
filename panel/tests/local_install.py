#!/usr/bin/env python3
"""Offline service files plus real controller/Agent verify automatic local enrollment."""
import os
from pathlib import Path
import signal
import sys
import tempfile

sys.path.insert(0, str(Path(__file__).resolve().parents[2] / 'scripts'))
from chain_common import verify_tree
from chain_install import Manager, make_parser, parse_env, provision_local_agent
from smoke import Harness


def main():
    source = Path(sys.argv[1]).resolve()
    manifest = verify_tree(source)
    os.environ['CHAIN_TEST_BIN_DIR'] = str(source / 'bin')
    os.environ['CHAIN_TEST_XRAY'] = str(source / 'bin/xray')
    with tempfile.TemporaryDirectory(prefix='chain-local-enroll-', dir=os.environ['CHAIN_TEST_ROOT']) as temporary:
        root = Path(temporary)
        harness = Harness(root)
        try:
            port = harness.port()
            harness.base = f'http://127.0.0.1:{port}'
            manager = Manager(root / 'system')
            args = make_parser().parse_args(['install', '--root', str(manager.root), '--listen', f'127.0.0.1:{port}',
                                             '--local-host', '127.0.0.1', '--local-target', 'localhost:443'])
            manager.install(source, manifest, args)
            panel_env = parse_env(manager.role_paths('panel')[0] / 'panel.env')
            harness.launch('local-panel', source / 'bin/chain-panel', panel_env)
            harness.wait(lambda: harness.request('/') or True, 'local panel start')
            provision_local_agent(manager, source, manifest, args)
            agent_env = parse_env(manager.role_paths('agent')[0] / 'agent.env')
            harness.launch('auto-agent', source / 'bin/chain-agent', agent_env)
            token = (manager.role_paths('panel')[1] / 'admin-token').read_text()
            harness.request('/api/login', 'POST', {'token': token})
            harness.wait(lambda: len(harness.state()['nodes']) == 1 and harness.state()['nodes'][0]['online'], 'automatically registered Agent online')
            previous = manager.state()['roles']['agent'].copy()
            provision_local_agent(manager, source, manifest, args)
            assert manager.state()['roles']['agent'] == previous
            assert len(harness.state()['nodes']) == 1
            print('PASS installed service configuration, real local auto-enrollment, node heartbeat, and repeated installation preserving identity.', flush=True)
        finally:
            harness.close()
    assert not root.exists()


if __name__ == '__main__':
    signal.signal(signal.SIGTERM, lambda *_: (_ for _ in ()).throw(KeyboardInterrupt()))
    main()
