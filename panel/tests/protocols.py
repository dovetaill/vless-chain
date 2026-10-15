#!/usr/bin/env python3
"""Create each supported protocol and exercise original exports with real cores."""
import http.server
import json
import os
from pathlib import Path
import signal
import shutil
import socket
import socketserver
import ssl
import subprocess
import tempfile

from smoke import Harness, HttpHandler, TlsServer, UdpEcho
from mihomo import exercise


def main():
    with tempfile.TemporaryDirectory(prefix='protocols-', dir=os.environ['CHAIN_TEST_ROOT']) as temporary:
        h = Harness(Path(temporary))
        try:
            cert, key = h.root / 'cert.pem', h.root / 'key.pem'
            subprocess.run(['openssl', 'req', '-x509', '-newkey', 'ec', '-pkeyopt',
                'ec_paramgen_curve:prime256v1', '-nodes', '-keyout', str(key), '-out', str(cert),
                '-days', '1', '-subj', '/CN=localhost', '-addext', 'subjectAltName=DNS:localhost'],
                check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            tls_port, panel_port, h.http_port, udp_port = [h.port() for _ in range(4)]
            target = TlsServer(('127.0.0.1', tls_port), HttpHandler)
            target.tls_context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
            target.tls_context.minimum_version = ssl.TLSVersion.TLSv1_3
            target.tls_context.set_alpn_protocols(['http/1.1'])
            target.tls_context.load_cert_chain(cert, key)
            h.server(target)
            h.server(http.server.ThreadingHTTPServer(('127.0.0.1', h.http_port), HttpHandler))
            h.server(socketserver.ThreadingUDPServer(('127.0.0.1', udp_port), UdpEcho))
            h.base = f'http://127.0.0.1:{panel_port}'
            h.launch('panel', h.bin_dir / 'chain-panel', {'CHAIN_DATA_DIR': str(h.root / 'control'),
                'CHAIN_LISTEN': f'127.0.0.1:{panel_port}', 'CHAIN_XRAY_BIN': str(h.xray), 'SSL_CERT_FILE': str(cert)})
            h.wait(lambda: h.request('/') or True, 'panel listener')
            h.login()
            node = h.request('/api/nodes', 'POST', {'name': 'protocol-test', 'host': '127.0.0.1',
                'target': f'127.0.0.1:{tls_port}', 'server_name': 'localhost'})
            token = h.root / 'token'
            token.write_text(node['token'])
            token.chmod(0o600)
            certbot = h.root / 'certbot'
            certbot.write_text('#!/bin/sh\nexit 0\n')
            certbot.chmod(0o700)
            agent_env = {'CHAIN_AGENT_DIR': str(h.root / 'agent'),
                'CHAIN_PANEL_URL': h.base, 'CHAIN_NODE_ID': node['id'], 'CHAIN_NODE_TOKEN_FILE': str(token),
                'CHAIN_XRAY_BIN': str(h.xray), 'SSL_CERT_FILE': str(cert),
                'CHAIN_CERTBOT_BIN': str(certbot), 'CHAIN_ACME_WEBROOT': str(h.root / 'acme-web')}
            agent = h.launch('agent', h.bin_dir / 'chain-agent', agent_env)
            h.wait(lambda: h.state()['nodes'][0]['online'], 'agent heartbeat')
            # Exercise the real automatic-certificate paths without contacting an ACME CA.
            live = h.root / 'agent/acme/config/live/localhost'
            live.mkdir(parents=True)
            shutil.copyfile(cert, live / 'fullchain.pem')
            shutil.copyfile(key, live / 'privkey.pem')
            # The isolated Agent shares this host with production; reserve its listeners.
            occupied = h.state()['nodes'][0]['report']['system']['listening_ports']
            for port in occupied:
                if 50000 <= port < 65535:
                    h.request('/api/lines', 'POST', {'name': f'reserved-{port}', 'kind': 'vless',
                        'enabled': False, 'hops': [{'node_id': node['id'], 'port': port}]})
            tls = {'mode': 'manual', 'domain': 'localhost', 'cert': cert.read_text(), 'key': key.read_text()}
            lines = []
            for protocol, secure in [('vless', False), ('shadowsocks', False), ('trojan', True),
                    ('http', False), ('socks', False), ('vmess', False), ('vmess', True),
                    ('hysteria2', True), ('tuic', True)]:
                access = {'protocol': protocol, 'host': '127.0.0.1', 'tls': tls if secure else {'mode': 'off'}}
                if protocol == 'tuic':
                    access['tls'] = {'mode': 'auto', 'domain': 'localhost'}
                line = h.request('/api/lines', 'POST', {'name': protocol + (' TLS' if secure else ''),
                    'kind': 'vless', 'hops': [{'node_id': node['id'], 'port': h.port()}], 'access': access})
                lines.append(line)
            for protocol in ['trojan', 'hysteria2', 'tuic']:
                h.request('/api/lines', 'POST', {'name': 'invalid TLS', 'kind': 'vless',
                    'hops': [{'node_id': node['id'], 'port': h.port()}], 'access': {'protocol': protocol}}, status=400)
            h.publish()
            for line in lines:
                h.probe(line)
                exported = h.request(f"/api/lines/{line['id']}/export")
                proxy = json.loads(exported['node'][2:])
                assert proxy['port'] == line['hops'][0]['port']
                if proxy['type'] == 'vless':
                    assert proxy['reality-opts']['public-key'] and proxy['reality-opts']['short-id']
                    assert proxy['servername'] == 'localhost' and proxy['flow'] == 'xtls-rprx-vision'
                for source in [exported['share'], exported['node']]:
                    imported = h.request('/api/externals', 'POST', {'url': source})
                    again = h.request(f"/api/externals/{imported['id']}/export")
                    assert json.loads(again['node'][2:])['type'] == proxy['type']
                print('PASS share link and Mihomo import round trip: ' + line['name'], flush=True)
            exercise(h, lines, udp_port)
            # Both positions exercise the TUIC sidecar and its UDP-capable dialer.
            tuic = lines[-1]
            vless = lines[0]
            chained = []
            for hops in [[tuic], [vless, tuic], [tuic, vless]]:
                line = h.request('/api/lines', 'POST', {'name': 'chain ' + ' '.join(x['name'] for x in hops),
                    'kind': 'vless', 'hops': [{'node_id': node['id'], 'port': h.port()}],
                    'access': {'protocol': 'socks', 'host': '127.0.0.1'},
                    'chain': ['line:' + x['id'] for x in hops]})
                chained.append(line)
            h.publish()
            # Xray prepares REALITY target records asynchronously after a restart.
            h.probe(vless)
            for line in chained:
                h.probe(line)
            exercise(h, chained, udp_port)
            print('PASS TUIC as landing, preceding hop and following hop', flush=True)
            active = json.loads((h.root / 'agent/active.json').read_text())
            backend = next(l['port'] for l in active['config']['_mihomo']['listeners'] if l['type'] == 'tuic')
            h.stop_process(agent)
            h.ports.add(backend)
            h.record()
            with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as occupied:
                occupied.bind(('127.0.0.1', backend))
                rejected = h.launch('agent-occupied', h.bin_dir / 'chain-agent', agent_env)
                assert rejected.wait(timeout=15) != 0
                assert 'Mihomo 监听启动失败' in (h.root / 'agent-occupied.log').read_text()
            print('PASS failed TUIC listener cannot be reported as running', flush=True)
        except BaseException:
            for log in h.root.rglob('*.log'):
                print(f'LOG {log.relative_to(h.root)}:\n{log.read_text(errors="replace")[-2200:]}', flush=True)
            raise
        finally:
            h.close()


if __name__ == '__main__':
    signal.signal(signal.SIGTERM, lambda *_: (_ for _ in ()).throw(KeyboardInterrupt()))
    main()
