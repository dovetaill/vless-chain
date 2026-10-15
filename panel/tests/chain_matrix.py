#!/usr/bin/env python3
"""Real exported-client traffic through every supported ingress and ordered outbound pair."""
import argparse
import http.client
import http.server
import json
import os
from pathlib import Path
import secrets
import signal
import socket
import socketserver
import subprocess
import tempfile
import urllib.request

from smoke import Harness, HttpHandler, UdpEcho
from mihomo import udp_echo


PROFILES = [
    ('reality-vision', 'vless', False, 'tcp'),
    ('tls-vision', 'vless', True, 'tcp'),
    ('reality-xhttp', 'vless', False, 'xhttp'),
    ('tls-xhttp', 'vless', True, 'xhttp'),
    ('ss2022', 'shadowsocks', False, 'tcp'),
    ('trojan', 'trojan', True, 'tcp'),
    ('http', 'http', False, 'tcp'),
    ('https', 'http', True, 'tcp'),
    ('socks5', 'socks', False, 'tcp'),
    ('vmess', 'vmess', False, 'tcp'),
    ('vmess-tls', 'vmess', True, 'tcp'),
    ('hysteria2', 'hysteria2', True, 'tcp'),
    ('tuic5', 'tuic', True, 'tcp'),
]


def main(cases=()):
    results = []
    def selected(label):
        return not cases or label in cases
    with tempfile.TemporaryDirectory(prefix='matrix-', dir=os.environ['CHAIN_TEST_ROOT']) as temporary:
        h = Harness(Path(temporary))
        try:
            def port():
                while True:
                    value = h.port()
                    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as check:
                        try:
                            check.bind(('127.0.0.1', value))
                            return value
                        except OSError:
                            h.ports.remove(value)
                            h.record()

            cert, key = h.root/'cert.pem', h.root/'key.pem'
            subprocess.run(['openssl', 'req', '-x509', '-newkey', 'ec', '-pkeyopt',
                'ec_paramgen_curve:prime256v1', '-nodes', '-keyout', str(key), '-out', str(cert),
                '-days', '1', '-subj', '/CN=localhost', '-addext', 'subjectAltName=DNS:localhost'],
                check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            tls_port, panel_port, h.http_port, udp_port, client_port, controller_port = [port() for _ in range(6)]
            h.server(http.server.ThreadingHTTPServer(('127.0.0.1', h.http_port), HttpHandler))
            udp_received = []
            class Echo(UdpEcho):
                def handle(self):
                    udp_received.append(len(self.request[0]))
                    super().handle()
            h.server(socketserver.ThreadingUDPServer(('127.0.0.1', udp_port), Echo))
            config = h.root/'nginx.conf'
            config.write_text(f'pid {h.root}/nginx.pid;\nworker_processes 1;\nerror_log {h.root}/nginx-error.log;\n'
                'events { worker_connections 512; }\nhttp { access_log off;\n'
                f'server {{ listen 127.0.0.1:{tls_port} ssl; http2 on; server_name localhost;\n'
                f'ssl_certificate {cert}; ssl_certificate_key {key}; ssl_protocols TLSv1.3;\n'
                "location / { return 200 'target-ok'; } } }\n")
            h.launch('nginx', 'nginx', {}, ['-p', str(h.root), '-c', str(config), '-g', 'daemon off;'])
            h.base = f'http://127.0.0.1:{panel_port}'
            h.launch('panel', h.bin_dir/'chain-panel', {'CHAIN_DATA_DIR': str(h.root/'control'),
                'CHAIN_LISTEN': f'127.0.0.1:{panel_port}', 'CHAIN_XRAY_BIN': str(h.xray), 'SSL_CERT_FILE': str(cert)})
            h.wait(lambda: h.request('/') or True, 'panel')
            h.login()
            node = h.request('/api/nodes', 'POST', {'name': 'chain-matrix', 'host': '127.0.0.1',
                'target': f'127.0.0.1:{tls_port}', 'server_name': 'localhost'})
            token = h.root/'token'; token.write_text(node['token']); token.chmod(0o600)
            h.launch('agent', h.bin_dir/'chain-agent', {'CHAIN_AGENT_DIR': str(h.root/'agent'),
                'CHAIN_PANEL_URL': h.base, 'CHAIN_NODE_ID': node['id'], 'CHAIN_NODE_TOKEN_FILE': str(token),
                'CHAIN_XRAY_BIN': str(h.xray), 'SSL_CERT_FILE': str(cert)})
            h.wait(lambda: h.state()['nodes'][0]['online'], 'agent')
            tls = {'mode': 'manual', 'domain': 'localhost', 'cert': cert.read_text(), 'key': key.read_text()}

            def create(name, profile, chain=()):
                _, protocol, secure, network = profile
                return h.request('/api/lines', 'POST', {'name': name, 'kind': 'vless',
                    'hops': [{'node_id': node['id'], 'port': port()}], 'chain': list(chain),
                    'access': {'protocol': protocol, 'host': '127.0.0.1', 'tls': tls if secure else {'mode': 'off'},
                        'transport': {'network': network, 'path': '/matrix', 'host': '', 'mode': 'auto'}}})

            exits = [create('exit-'+p[0], p) for p in PROFILES]
            middles = [create('middle-'+p[0], p) for p in PROFILES]
            entries = [create('entry-'+p[0], p, ['line:'+exits[1]['id']]) for p in PROFILES]
            drivers = [create('driver-'+p[0], PROFILES[8]) for p in PROFILES]
            occupied = h.state()['nodes'][0]['report']['system']['listening_ports']
            line_ports = {l['hops'][0]['port'] for l in exits+middles+entries+drivers}
            for reserved in (set(occupied) | h.ports) - line_ports:
                if 50000 <= reserved < 65535:
                    h.request('/api/lines', 'POST', {'name': f'reserved-{reserved}', 'kind': 'vless',
                        'enabled': False, 'hops': [{'node_id': node['id'], 'port': reserved}]})
            h.publish()
            middle_refs, exit_refs, proxies = [], [], []
            for group, refs in [(middles, middle_refs), (exits, exit_refs)]:
                for i, line in enumerate(group):
                    export = h.request('/api/lines/'+line['id']+'/export')
                    # Exercise both URI and Mihomo-node imports in the actual outbound chain.
                    external = h.request('/api/externals', 'POST', {'url': export['share'] if i % 2 else export['node']})
                    refs.append('external:'+external['id'])
            for line in entries+drivers:
                export = h.request('/api/lines/'+line['id']+'/export')
                proxies.append(json.loads(export['node'][2:]))
            secret = secrets.token_hex(16)
            client_config = {'mixed-port': client_port, 'bind-address': '127.0.0.1', 'mode': 'rule',
                'external-controller': f'127.0.0.1:{controller_port}', 'secret': secret,
                'proxies': proxies, 'proxy-groups': [{'name': 'matrix', 'type': 'select', 'proxies': [p['name'] for p in proxies]}],
                'rules': ['MATCH,matrix'], 'log-level': 'warning'}
            client_dir = h.root/'client'; client_dir.mkdir()
            client_path = client_dir/'config.json'; client_path.write_text(json.dumps(client_config)); client_path.chmod(0o600)
            checked = subprocess.run([os.environ['CHAIN_TEST_MIHOMO'], '-t', '-d', str(client_dir), '-f', str(client_path)],
                env={**os.environ, 'SSL_CERT_FILE': str(cert)}, capture_output=True, text=True, timeout=20)
            assert checked.returncode == 0, checked.stdout+checked.stderr
            h.launch('client', os.environ['CHAIN_TEST_MIHOMO'], {'SSL_CERT_FILE': str(cert)}, ['-d', str(client_dir), '-f', str(client_path)])

            def select(name):
                request = urllib.request.Request(f'http://127.0.0.1:{controller_port}/proxies/matrix',
                    method='PUT', headers={'Authorization': 'Bearer '+secret, 'Content-Type': 'application/json'},
                    data=json.dumps({'name': name}).encode())
                with urllib.request.urlopen(request, timeout=5) as response:
                    assert response.status == 204
                return True
            h.wait(lambda: select(entries[0]['name']), 'Mihomo exported-client listener')

            def counters(ids):
                return {l['id']: (l.get('traffic') or {}).get('upload', 0)+(l.get('traffic') or {}).get('download', 0)
                        for l in h.state()['lines'] if l['id'] in ids}

            def test(label, entry, path, udp=True, udp_bytes=4096):
                select(entry['name'])
                if not udp and entry['access']['protocol'] != 'http':
                    exported = h.request('/api/lines/'+entry['id']+'/export')
                    assert json.loads(exported['node'][2:])['udp'] is False, 'TCP-only chain advertises UDP'
                    assert 'udp=false' in exported['share'], 'TCP-only SOCKS URI lost capability'
                ids = [entry['id'], *(l['id'] for l in path)]
                before = counters(ids)
                logs = [h.root/'agent/xray.log', h.root/'agent/mihomo/mihomo.log']
                offsets = {p: p.stat().st_size for p in logs if p.exists()}
                result = {'case': label, 'tcp': False, 'udp': None if not udp else False, 'hops': False, 'errors': []}
                def check_hops(stage, before):
                    def traversed():
                        after = counters(ids)
                        return set(after) == set(ids) and all(value > before.get(id, 0) for id, value in after.items())
                    try:
                        h.wait(traversed, stage+' all expected hop counters increased', timeout=8)
                        result[stage+'_hops'] = True
                    except Exception:
                        after = counters(ids)
                        result[stage+'_hop_bytes'] = {line['name']: after.get(line['id'], 0)-before.get(line['id'], 0)
                                                     for line in [entry, *path]}
                        raise
                def tcp_request():
                    connection = http.client.HTTPConnection('127.0.0.1', client_port, timeout=12)
                    try:
                        connection.request('GET', f'http://127.0.0.1:{h.http_port}/', headers={'Connection': 'close'})
                        response = connection.getresponse()
                        assert response.status == 200, 'HTTP status '+str(response.status)
                        body = response.read()
                        assert body == b'chain-integration-ok', 'HTTP content differs: '+repr(body[:160])
                    finally:
                        connection.close()
                try:
                    tcp_request()
                    result['tcp'] = True
                    check_hops('tcp', before)
                except Exception as error:
                    result['errors'].append('TCP '+str(error))
                if udp:
                    received_before = len(udp_received)
                    try:
                        before = counters(ids)
                        udp_echo(client_port, udp_port)
                        result['udp_small'] = True
                        udp_echo(client_port, udp_port, os.urandom(udp_bytes))
                        result['udp_payload_bytes'] = udp_bytes
                        result['udp'] = True
                        check_hops('udp', before)
                        if udp_bytes < 4096:
                            # sing-quic Hysteria 2 caps the encapsulated datagram at 4096 bytes.
                            try:
                                udp_echo(client_port, udp_port, os.urandom(udp_bytes+1))
                            except socket.timeout:
                                result['udp_over_limit_rejected'] = True
                            else:
                                raise AssertionError('Hysteria 2 UDP limit changed; update the boundary test')
                    except Exception as error:
                        result['errors'].append('UDP '+str(error))
                        result['udp_received'] = udp_received[received_before:]
                result['hops'] = bool(result.get('tcp_hops') and (not udp or result.get('udp_hops')))
                results.append(result)
                if result['errors'] and os.environ.get('CHAIN_XRAY_LOG_LEVEL') == 'info':
                    for log, offset in offsets.items():
                        with log.open() as stream:
                            stream.seek(offset)
                            diagnostic = [line.strip() for line in stream.read(65536).splitlines()
                                if cases or any(word in line.lower() for word in ['failed', 'timeout', 'unsupported', 'invalid', 'error'])]
                        if diagnostic:
                            print('CASE_DIAGNOSTIC '+json.dumps({'case': label, 'core': log.name, 'messages': diagnostic[-40:]}), flush=True)
                print('CASE '+json.dumps(result), flush=True)
                report = 'matrix-results-selected.json' if cases else 'matrix-results.json'
                (Path(os.environ['CHAIN_TEST_ROOT'])/report).write_text(json.dumps(results, indent=2))

            if 'exports and probes' in cases:
                for index in [4, 7, 11, 12]:
                    h.probe(entries[index])
                    print('PASS panel HTTP probe '+PROFILES[index][0], flush=True)
                limited = h.request('/api/lines', 'POST', {**drivers[0], 'chain': [exit_refs[6]]})
                nested = h.request('/api/lines', 'POST', {**drivers[1], 'chain': ['line:'+limited['id']]})
                h.publish()
                exported = h.request('/api/lines/'+nested['id']+'/export')
                assert json.loads(exported['node'][2:])['udp'] is False
                imported = h.request('/api/externals', 'POST', {'url': exported['node']})
                reexported = h.request('/api/externals/'+imported['id']+'/export')
                assert json.loads(reexported['node'][2:])['udp'] is False
                subscription = h.request('/api/subscriptions', 'POST', {'name': 'nested only', 'line_ids': [nested['id']]})
                yaml = h.request(subscription['path']).decode()
                assert nested['name'] in yaml and 'udp: false' in yaml
                error = h.request('/api/lines', 'POST', {**drivers[2], 'chain': ['line:'+limited['id'], exit_refs[11]]}, status=400)
                assert '需要 UDP' in error['error'], error
                results.append({'case': 'exports and probes', 'tcp': None, 'udp': None, 'hops': None, 'errors': []})
                print('PASS TCP-only nested export, import, subscription and QUIC rejection', flush=True)
            for profile, entry in zip(PROFILES, entries):
                label = 'ingress '+profile[0]+' -> tls-vision'
                if selected(label):
                    test(label, entry, [exits[1]], profile[1] != 'http')
            for b, profile in enumerate(PROFILES):
                label = 'single '+profile[0]
                if label in cases:
                    driver = h.request('/api/lines', 'POST', {**drivers[0], 'chain': [exit_refs[b]]})
                    h.publish()
                    test(label, driver, [exits[b]], profile[1] != 'http')
            for b, exit_line in enumerate(exits):
                indices = [a for a in range(len(PROFILES)) if selected(PROFILES[a][0]+' -> '+PROFILES[b][0])]
                if not indices:
                    continue
                valid = []
                for a in indices:
                    driver = drivers[a]
                    if PROFILES[a][1] == 'http' and PROFILES[b][1] in ('hysteria2', 'tuic'):
                        error = h.request('/api/lines', 'POST', {**driver, 'chain': [middle_refs[a], exit_refs[b]]}, status=400)
                        assert '需要 UDP' in error['error'], error
                        saved = next(l for l in h.state()['lines'] if l['id'] == driver['id'])
                        assert saved['chain'] == driver['chain'], 'Rejected chain changed saved state'
                        result = {'case': PROFILES[a][0]+' -> '+PROFILES[b][0], 'rejected': True,
                                  'tcp': None, 'udp': None, 'hops': None, 'errors': []}
                        results.append(result)
                        print('CASE '+json.dumps(result), flush=True)
                        continue
                    drivers[a] = h.request('/api/lines', 'POST', {**driver, 'chain': [middle_refs[a], exit_refs[b]]})
                    valid.append(a)
                if valid:
                    h.publish()
                for a in valid:
                    # IPv4 SS2022 AES response adds 58 bytes; SOCKS5 adds 10 bytes.
                    udp_bytes = {('hysteria2', 'shadowsocks'): 4096-58,
                                 ('hysteria2', 'socks'): 4096-10}.get((PROFILES[a][1], PROFILES[b][1]), 4096)
                    test(PROFILES[a][0]+' -> '+PROFILES[b][0], drivers[a], [middles[a], exit_line],
                         PROFILES[b][1] != 'http' and not (PROFILES[a][1] == 'http' and PROFILES[b][1] in ('shadowsocks', 'socks')), udp_bytes)
                print('COLUMN_DONE '+PROFILES[b][0], flush=True)
            for order in [[4, 2, 11, 12], [12, 11, 3, 5]]:
                label = 'mixed '+ ' -> '.join(PROFILES[i][0] for i in order)
                if not selected(label):
                    continue
                driver = h.request('/api/lines', 'POST', {**drivers[0], 'chain': [middle_refs[i] for i in order]})
                h.publish()
                test(label, driver, [middles[i] for i in order])
            failed = [r for r in results if r['errors']]
            assert not cases or set(cases) == {r['case'] for r in results}, 'Unknown or unexecuted case label'
            print('MATRIX_SUMMARY '+json.dumps({'total': len(results), 'tcp_pass': sum(r['tcp'] is True for r in results),
                'invalid_chains_rejected': sum(r.get('rejected', False) for r in results),
                'udp_pass': sum(r['udp'] is True for r in results), 'udp_not_applicable': sum(r['udp'] is None for r in results),
                'all_hops_pass': sum(r['hops'] is True for r in results), 'failed': failed}), flush=True)
            if failed:
                for log in [h.root/'client.log', h.root/'agent/xray.log', h.root/'agent/mihomo/mihomo.log']:
                    if log.exists(): print('LOG '+log.name+'\n'+log.read_text(errors='replace')[-5000:], flush=True)
            return not failed
        except Exception:
            for log in [h.root/'agent.log', h.root/'agent/xray.log', h.root/'agent/mihomo/mihomo.log']:
                if log.exists():
                    print('FAILURE_LOG '+log.name+'\n'+log.read_text(errors='replace')[-4000:], flush=True)
            raise
        finally:
            h.close()


if __name__ == '__main__':
    signal.signal(signal.SIGTERM, lambda *_: (_ for _ in ()).throw(KeyboardInterrupt()))
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--case', action='append', default=[], help='Run only this exact case; repeatable for targeted diagnosis')
    args = parser.parse_args()
    raise SystemExit(0 if main(args.case) else 1)
