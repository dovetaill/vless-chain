#!/usr/bin/env python3
"""Native v3 import: four original clients, three external VLESS exits, real traffic."""
import base64
import copy
import http.server
import json
import os
from pathlib import Path
import signal
import ssl
import subprocess
import tempfile

from smoke import Harness, HttpHandler, TlsServer


class Response(HttpHandler):
    def do_GET(self):
        body = b'chain-integration-ok'
        self.send_response(200)
        self.send_header('Content-Length', str(len(body)))
        self.end_headers()
        self.wfile.write(body)


def public_key(private):
    raw = base64.urlsafe_b64decode(private + '=')
    der = subprocess.check_output(['openssl', 'pkey', '-inform', 'DER', '-pubout', '-outform', 'DER'],
                                  input=bytes.fromhex('302e020100300506032b656e04220420') + raw,
                                  stderr=subprocess.DEVNULL)
    return base64.urlsafe_b64encode(der[-32:]).decode().rstrip('=')


def main():
    with tempfile.TemporaryDirectory(prefix='legacy-migration-', dir=os.environ['CHAIN_TEST_ROOT']) as temporary:
        h = Harness(Path(temporary))
        try:
            cert, key = h.root / 'cert.pem', h.root / 'key.pem'
            subprocess.run(['openssl', 'req', '-x509', '-newkey', 'ec', '-pkeyopt',
                'ec_paramgen_curve:prime256v1', '-nodes', '-keyout', str(key), '-out', str(cert),
                '-days', '1', '-subj', '/CN=localhost', '-addext', 'subjectAltName=DNS:localhost'],
                check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            tls_port = h.port()
            tls = TlsServer(('127.0.0.1', tls_port), HttpHandler)
            tls.tls_context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
            tls.tls_context.minimum_version = ssl.TLSVersion.TLSv1_3
            tls.tls_context.set_alpn_protocols(['http/1.1'])
            tls.tls_context.load_cert_chain(cert, key)
            h.server(tls)
            h.http_port = h.port()
            h.allow_loopback_target(h.http_port)
            h.server(http.server.ThreadingHTTPServer(('127.0.0.1', h.http_port), Response))
            panel_port = h.port()
            h.base = f'http://127.0.0.1:{panel_port}'
            h.launch('panel', h.bin_dir / 'chain-panel', {'CHAIN_DATA_DIR': str(h.root / 'control'),
                'CHAIN_LISTEN': f'127.0.0.1:{panel_port}', 'CHAIN_XRAY_BIN': str(h.xray)})
            h.wait(lambda: h.request('/') or True, 'panel ready')
            h.login()
            node = h.request('/api/nodes', 'POST', {'name': 'migration', 'host': '127.0.0.1',
                'target': f'127.0.0.1:{tls_port}', 'server_name': 'localhost'})
            token = h.root / 'token'
            token.write_text(node['token'])
            token.chmod(0o600)
            h.fixture_reality_target(tls_port, enabled=False)
            h.launch('agent', h.bin_dir / 'chain-agent', {'CHAIN_AGENT_DIR': str(h.root / 'agent'),
                'CHAIN_PANEL_URL': h.base, 'CHAIN_NODE_ID': node['id'], 'CHAIN_NODE_TOKEN_FILE': str(token),
                'CHAIN_XRAY_BIN': str(h.xray), 'CHAIN_XRAY_LOG_LEVEL': 'debug'})
            h.wait(lambda: h.state()['nodes'][0]['online'], 'Agent heartbeat')
            sources = json.loads(Path(__file__).with_name('fixtures').joinpath('native-v3.json').read_text())
            exits = []
            for index, source in enumerate(sources):
                inbound = source['config']['inbounds'][0]
                inbound['port'] = h.port()
                reality = inbound['streamSettings']['realitySettings']
                reality.update(target=f'127.0.0.1:{tls_port}', serverNames=['localhost'])
                if index:
                    exit_port = h.port()
                    exit_inbound = copy.deepcopy(inbound)
                    exit_inbound.update(listen='127.0.0.1', port=exit_port)
                    upstream = source['config']['outbounds'][0]
                    upstream['settings'].update(address='127.0.0.1', port=exit_port)
                    exit_inbound['settings']['clients'][0]['id'] = upstream['settings']['id']
                    upstream['streamSettings']['realitySettings'].update(serverName='localhost',
                        publicKey=public_key(reality['privateKey']), shortId=reality['shortIds'][0])
                    access_log = h.root / f'exit-{index}-access.log'
                    config = h.root / f'exit-{index}.json'
                    config.write_text(json.dumps({'log': {'access': str(access_log), 'loglevel': 'warning'},
                        'inbounds': [exit_inbound], 'outbounds': [{'protocol': 'freedom'}]}))
                    h.launch(f'exit-{index}', h.xray, {}, ['run', '-config', str(config)])
                    exits.append(access_log)
            request = {'node_id': node['id'], 'sources': sources, 'preview': True}
            preview = h.request('/api/legacy-import', 'POST', request)
            assert len(preview['imported']) == 4 and not preview['pending'], preview
            assert not h.state()['lines'] and not h.state()['externals']
            request['preview'] = False
            assert h.request('/api/legacy-import', 'POST', request) == preview
            state = h.state()
            assert len(state['lines']) == 4 and len(state['externals']) == 3
            assert len({line['name'] for line in state['lines']}) == 4
            h.request('/api/legacy-import', 'POST', request)
            assert len(h.state()['externals']) == 3
            # The real admission check must reject our private, self-signed cover.
            before_jobs = h.state()['jobs']
            warning = h.request('/api/publish', 'POST', {}, status=400)
            assert 'REALITY' in warning['error'] and '草稿' in warning['error'], warning
            h.request('/api/publish', 'POST', {'allow_unverified_reality': True}, status=422)
            assert h.state()['jobs'] == before_jobs
            assert len(h.state()['lines']) == 4
            h.preflight_fixture.enabled = True
            h.publish()
            assert h.preflight_fixture.intercepted > 0
            for index, (source, line) in enumerate(zip(sources, state['lines'])):
                inbound = source['config']['inbounds'][0]
                reality = inbound['streamSettings']['realitySettings']
                assert line['hops'][0]['uuid'] == inbound['settings']['clients'][0]['id']
                assert line['hops'][0]['port'] == inbound['port']
                # Build the client from the old configuration, independently of the new export.
                client_port = h.port()
                config = h.root / f'client-{index}.json'
                config.write_text(json.dumps({'log': {'loglevel': 'debug'}, 'inbounds': [
                    {'listen': '127.0.0.1', 'port': client_port, 'protocol': 'socks', 'settings': {'auth': 'noauth'}}],
                    'outbounds': [{'protocol': 'vless', 'settings': {'vnext': [{'address': '127.0.0.1',
                        'port': inbound['port'], 'users': [{**inbound['settings']['clients'][0], 'encryption': 'none'}]}]},
                        'streamSettings': {'network': 'tcp', 'security': 'reality', 'realitySettings': {
                            'serverName': 'localhost', 'publicKey': public_key(reality['privateKey']),
                            'shortId': reality['shortIds'][0], 'fingerprint': 'chrome'}}}]}))
                client = h.launch(f'client-{index}', h.xray, {}, ['run', '-config', str(config)])
                last_error = ''
                def probe():
                    nonlocal last_error
                    result = subprocess.run(['curl', '--noproxy', '', '--socks5-hostname', f'127.0.0.1:{client_port}',
                        '--max-time', '3', '-fsS', f'http://127.0.0.1:{h.http_port}/'], capture_output=True)
                    last_error = result.stderr.decode(errors='replace')
                    return result.returncode == 0 and result.stdout == b'chain-integration-ok'
                try:
                    h.wait(probe, f'original client {index} traffic', timeout=20)
                except AssertionError:
                    print('Last client error:', last_error, flush=True)
                    for path in [h.root / f'client-{index}.log', h.root / 'agent/xray.log', h.root / 'agent.log']:
                        if path.exists():print(path.name, path.read_text(errors='replace')[-6000:], flush=True)
                    raise
                if index:
                    h.wait(lambda: exits[index-1].exists() and f'127.0.0.1:{h.http_port}' in exits[index-1].read_text(),
                           f'traffic reached external exit {index}')
                h.stop_process(client)
            bad = copy.deepcopy(request)
            bad['preview'] = True
            bad['sources'] = [copy.deepcopy(sources[1])]
            bad['sources'][0]['id'] += ':unsupported'
            bad['sources'][0]['config']['outbounds'][0]['mux'] = {'enabled': True}
            result = h.request('/api/legacy-import', 'POST', bad)
            assert len(result['pending']) == 1 and not result['imported']
            assert len(h.state()['externals']) == 3 and not h.state()['pending_imports']
            print('PASS four native instances; three external VLESS exits; unchanged original clients carry real traffic; preview and retry preserve state', flush=True)
        finally:
            h.close()
    assert not Path(temporary).exists()


if __name__ == '__main__':
    signal.signal(signal.SIGTERM, lambda *_: (_ for _ in ()).throw(KeyboardInterrupt()))
    main()
