#!/usr/bin/env python3
"""Local identity recovery, ordered proxy traffic and shared Nginx HTTP-01."""
import hashlib
import http.server
import json
import os
from pathlib import Path
import secrets
import signal
import subprocess
import sys
import tempfile
import uuid

from smoke import Harness, HttpHandler

sys.path.insert(0, str(Path(__file__).resolve().parents[2] / 'scripts'))



def main():
    with tempfile.TemporaryDirectory(prefix='proxy-flow-', dir=os.environ['CHAIN_TEST_ROOT']) as temporary:
        root = Path(temporary)
        h = Harness(root)
        try:
            cert, key = root / 'cert.pem', root / 'key.pem'
            subprocess.run(['openssl', 'req', '-x509', '-newkey', 'ec', '-pkeyopt',
                'ec_paramgen_curve:prime256v1', '-nodes', '-keyout', str(key), '-out', str(cert),
                '-days', '1', '-subj', '/CN=localhost', '-addext', 'subjectAltName=DNS:localhost'],
                check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            data = root / 'control'
            data.mkdir(mode=0o700)
            (data / '.chain-owner').write_text('chain-panel')
            node_id, token = str(uuid.uuid4()), secrets.token_hex(32)
            (data / 'local-system-node').write_text(node_id)
            (data / 'local-system-identity.json').write_text(json.dumps({
                'id': node_id, 'token_hash': hashlib.sha256(token.encode()).hexdigest()}))
            token_file = root / 'token'
            token_file.write_text(token)
            token_file.chmod(0o600)
            # Nginx owns a high loopback port, using the same generated challenge routes.
            web = root / 'web'
            web.mkdir()
            http_port = h.port()
            conf = root / 'nginx.conf'
            conf.write_text(f'pid {root}/nginx.pid;\nerror_log {root}/nginx.log;\nevents {{}}\n'
                'http { access_log off;\n' + ''.join(f'{name}_temp_path {root}/{name};\n' for name in ('client_body','proxy','fastcgi','uwsgi','scgi')) + subprocess.check_output([str(h.bin_dir / 'chainctl'), 'internal-render-acme', '--root', str(root)], text=True).replace('listen 80;', f'listen 127.0.0.1:{http_port};')
                .replace(str(root / 'var/lib/chain-acme'), str(web)) + '}\n')
            h.launch('nginx', 'nginx', {}, ['-p', str(root), '-c', str(conf), '-g', 'daemon off; master_process off;'])
            fake = root / 'certbot'
            fake.write_text('''#!/usr/bin/env python3
import os, sys, pathlib, urllib.request, shutil
args=sys.argv[1:]
def arg(name): return args[args.index(name)+1]
web=pathlib.Path(arg('--webroot-path'))
challenge=web/'.well-known/acme-challenge/owned-probe'
challenge.parent.mkdir(parents=True,exist_ok=True)
challenge.write_text('owned-challenge')
try:
    assert urllib.request.urlopen(os.environ['TEST_CHALLENGE_URL'],timeout=5).read()==b'owned-challenge'
finally:
    challenge.unlink()
live=pathlib.Path(arg('--config-dir'))/'live'/arg('--cert-name')
live.mkdir(parents=True,exist_ok=True)
shutil.copyfile(os.environ['TEST_CERT'],live/'fullchain.pem')
shutil.copyfile(os.environ['TEST_KEY'],live/'privkey.pem')
''')
            fake.chmod(0o700)
            h.http_port = h.port()
            h.allow_loopback_target(h.http_port)
            h.server(http.server.ThreadingHTTPServer(('127.0.0.1', h.http_port), HttpHandler))
            panel_port = h.port()
            h.base = f'http://127.0.0.1:{panel_port}'
            h.launch('panel', h.bin_dir / 'chain-panel', {'CHAIN_DATA_DIR': str(data),
                'CHAIN_LISTEN': f'127.0.0.1:{panel_port}', 'CHAIN_XRAY_BIN': str(h.xray),
                'SSL_CERT_FILE': str(cert)})
            h.wait(lambda: (data / 'admin-token').exists(), 'panel token')
            h.wait(lambda: h.request('/') or True, 'panel listener')
            h.login()
            assert h.state()['local_node_id'] is None
            assert h.request('/api/local-node', 'POST', {'host': '127.0.0.1'})['id'] == node_id
            h.launch('agent', h.bin_dir / 'chain-agent', {'CHAIN_AGENT_DIR': str(root / 'agent'),
                'CHAIN_PANEL_URL': h.base, 'CHAIN_NODE_ID': node_id, 'CHAIN_NODE_TOKEN_FILE': str(token_file),
                'CHAIN_XRAY_BIN': str(h.xray), 'CHAIN_ACME_WEBROOT': str(web / 'agent'),
                'CHAIN_CERTBOT_BIN': str(fake), 'TEST_CHALLENGE_URL': f'http://127.0.0.1:{http_port}/.well-known/acme-challenge/owned-probe',
                'TEST_CERT': str(cert), 'TEST_KEY': str(key), 'SSL_CERT_FILE': str(cert)})
            h.wait(lambda: h.state()['nodes'][0]['online'], 'restored Agent heartbeat')
            assert h.request('/api/local-node', 'POST', {'host': '127.0.0.1'})['id'] == node_id
            assert len(h.state()['nodes']) == 1
            print('PASS stale local identity restored without changing Agent credentials', flush=True)
            def create(name, protocol='vless', **extra):
                return h.request('/api/lines', 'POST', {'name': name, 'kind': 'vless',
                    'hops': [{'node_id': node_id, 'port': h.port()}],
                    'access': {'protocol': protocol, 'host': '127.0.0.1'}, **extra})
            tls = {'mode': 'auto', 'domain': 'localhost'}
            a = create('Landing A', access={'protocol': 'vless', 'host': '127.0.0.1', 'tls': tls})
            b = create('Landing B', access={'protocol': 'vless', 'host': '127.0.0.1', 'tls': tls})
            h.publish()
            h.probe(a)
            print('PASS automatic certificate path through real Nginx with fixture CA', flush=True)
            externals = []
            for line in [a, b]:
                exported = h.request(f"/api/lines/{line['id']}/export")
                node = json.loads(exported['node'].strip()[2:])
                node['skip-cert-verify'] = False
                e = h.request('/api/externals', 'POST', {'url': '- ' + json.dumps(node)})
                both = h.request(f"/api/externals/{e['id']}/export")
                assert both['share'].startswith('vless://')
                assert json.loads(both['node'].strip()[2:])['skip-cert-verify'] is False
                externals.append(e)
            chained = create('Ordered A B', 'http', chain=['external:' + e['id'] for e in externals])
            local = create('Local reference', 'socks', chain=['line:' + a['id']])
            h.publish()
            before = {l['id']: l['traffic']['upload'] + l['traffic']['download'] for l in h.state()['lines'] if l['id'] in [a['id'], b['id']]}
            h.probe(chained)
            h.probe(local)
            h.wait(lambda: all(l['traffic']['upload'] + l['traffic']['download'] > before[l['id']]
                for l in h.state()['lines'] if l['id'] in before), 'both landing counters increased')
            print('PASS real Xray traffic through A then B and through referenced local proxy', flush=True)
            h.request(f"/api/externals/{externals[0]['id']}", 'DELETE', status=400)
            h.request(f"/api/lines/{a['id']}", 'DELETE', status=400)
            h.request('/api/lines', 'POST', {**a, 'chain': ['line:' + local['id']]}, status=400)
            h.request('/api/lines', 'POST', {**a, 'kind': 'forward'}, status=400)
            h.request('/api/lines', 'POST', {**a, 'access': {**a['access'], 'public_host': 'relay.example'}}, status=400)
            print('PASS cycle/deletion checks and removed forwarding/purchased entry rejection', flush=True)
        finally:
            h.close()
    assert not root.exists()


if __name__ == '__main__':
    signal.signal(signal.SIGTERM, lambda *_: (_ for _ in ()).throw(KeyboardInterrupt()))
    main()
