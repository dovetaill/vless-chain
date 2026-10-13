#!/usr/bin/env python3
"""Real Nginx -> panel HTTPS login and HTTP-01, on isolated loopback ports."""
import http.cookiejar
import json
import os
from pathlib import Path
import signal
import ssl
import subprocess
import sys
import tempfile
import urllib.error
import urllib.request

sys.path.insert(0, str(Path(__file__).resolve().parents[2] / 'scripts'))
from chain_network import HttpsSite
from smoke import Harness


def main():
    source = Path(sys.argv[1]).resolve()
    os.environ['CHAIN_TEST_BIN_DIR'] = str(source / 'bin')
    os.environ['CHAIN_TEST_XRAY'] = str(source / 'bin/xray')
    with tempfile.TemporaryDirectory(prefix='chain-https-integration-', dir=os.environ['CHAIN_TEST_ROOT']) as temporary:
        root = Path(temporary)
        harness = Harness(root)
        try:
            upstream, http_port, https_port = harness.port(), harness.port(), harness.port()
            harness.base = f'http://127.0.0.1:{upstream}'
            harness.launch('panel', source / 'bin/chain-panel', {
                'CHAIN_DATA_DIR': str(root / 'data'), 'CHAIN_LISTEN': f'127.0.0.1:{upstream}',
                'CHAIN_SECURE_COOKIE': '1', 'CHAIN_XRAY_BIN': str(source / 'bin/xray')})
            harness.wait(lambda: harness.request('/') or True, 'panel started')
            site = HttpsSite('localhost', upstream)
            site.webroot, site.cert = root / 'webroot', root / 'cert'
            site.cert.mkdir()
            proc = subprocess.Popen(['openssl', 'req', '-x509', '-newkey', 'rsa:2048', '-nodes', '-days', '1',
                                     '-subj', '/CN=localhost', '-addext', 'subjectAltName=DNS:localhost',
                                     '-keyout', str(site.cert / 'privkey.pem'), '-out', str(site.cert / 'fullchain.pem')],
                                    stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, start_new_session=True)
            log = (root / 'openssl.log').open('wb')
            harness.processes.append(('openssl', proc, log))
            harness.record()
            assert proc.wait() == 0
            challenge = site.webroot / '.well-known/acme-challenge/probe'
            challenge.parent.mkdir(parents=True)
            challenge.write_text('owned-challenge')
            conf = root / 'nginx.conf'
            # Single process stays in the owned group; no host paths/listeners.
            conf.write_text(f'pid {root}/nginx.pid;\nerror_log {root}/error.log;\nevents {{}}\n'
                            'http { access_log off;\n' + ''.join(f'{name}_temp_path {root}/{name};\n'
                                for name in ('client_body', 'proxy', 'fastcgi', 'uwsgi', 'scgi')) +
                            site.config(tls=True).replace('listen 80;', f'listen 127.0.0.1:{http_port};').replace(
                                'listen 443 ssl;', f'listen 127.0.0.1:{https_port} ssl;') + '}\n')
            nginx_log = (root / 'nginx.log').open('wb')
            nginx = subprocess.Popen(['nginx', '-p', str(root), '-c', str(conf), '-e', str(root / 'error.log'),
                                      '-g', 'daemon off; master_process off;'], stdout=nginx_log, stderr=nginx_log,
                                     start_new_session=True)
            harness.processes.append(('nginx', nginx, nginx_log))
            harness.record()
            context = ssl.create_default_context(cafile=str(site.cert / 'fullchain.pem'))
            cookies = http.cookiejar.CookieJar()
            client = urllib.request.build_opener(urllib.request.ProxyHandler({}), urllib.request.HTTPSHandler(context=context),
                                                urllib.request.HTTPCookieProcessor(cookies))
            base = f'https://localhost:{https_port}'
            def ready():
                try:
                    with client.open(base, timeout=1) as response:
                        return response.status == 200 and b'<!doctype html>' in response.read(100).lower()
                except OSError:
                    return False
            harness.wait(ready, 'HTTPS reverse proxy ready')
            with client.open(f'http://127.0.0.1:{http_port}/.well-known/acme-challenge/probe') as response:
                assert response.read() == b'owned-challenge'
            token = (root / 'data/admin-token').read_text().strip()
            request = urllib.request.Request(base + '/api/login', data=json.dumps({'token': token}).encode(),
                                             headers={'Content-Type': 'application/json', 'X-Chain-Request': '1',
                                                      'Origin': base})
            with client.open(request) as response:
                assert response.status == 200
                assert 'Secure' in response.headers['Set-Cookie']
            with client.open(urllib.request.Request(base + '/api/state', headers={'X-Chain-Request': '1'})) as response:
                assert response.status == 200
            print('PASS real HTTPS page, trusted TLS certificate, secure-cookie login, authenticated API and HTTP-01 route.', flush=True)
        finally:
            harness.close()
    assert not root.exists()


if __name__ == '__main__':
    signal.signal(signal.SIGTERM, lambda *_: (_ for _ in ()).throw(KeyboardInterrupt()))
    main()
