"""Optional compatibility check using a separately verified official Mihomo binary."""
import http.client
import json
import os
import subprocess


def exercise(h, lines):
    vless = next(l for l in h.state()['lines'] if l['kind'] == 'vless' and l['access']['protocol'] == 'vless')
    for index, line in enumerate([vless, *lines]):
        export = h.request(f"/api/lines/{line['id']}/export")
        directory = h.root / f'mihomo-{index}'
        directory.mkdir()
        port = h.port()
        assert 'config' not in export and export['node'].startswith('- ')
        node = json.loads(export['node'][2:])
        config = (f'mixed-port: {port}\nmode: rule\nproxies:\n'
                  f"  {export['node'].strip()}\nrules:\n  - {json.dumps('MATCH,' + node['name'])}\n")
        path = directory / 'config.yaml'
        path.write_text(config)
        # The fixture's private CA is trusted only by this test-owned process.
        env = {**os.environ, 'SSL_CERT_FILE': str(h.root / 'cert.pem')}
        result = subprocess.run([os.environ['CHAIN_TEST_MIHOMO'], '-t', '-f', str(path), '-d', str(directory)],
                                env=env, capture_output=True, text=True, timeout=20)
        assert result.returncode == 0, result.stdout + result.stderr
        process = h.launch(f'mihomo-{index}', os.environ['CHAIN_TEST_MIHOMO'], env,
                           ['-f', str(path), '-d', str(directory)])
        try:
            def request():
                connection = http.client.HTTPConnection('127.0.0.1', port, timeout=10)
                try:
                    connection.request('GET', f'http://127.0.0.1:{h.http_port}/')
                    response = connection.getresponse()
                    assert response.status == 200
                    assert response.read() == b'chain-integration-ok'
                    return True
                finally:
                    connection.close()
            h.wait(request, 'Mihomo listener and exported proxy connection')
        finally:
            h.stop_process(process)
    print('PASS exported proxy YAML parsed and used by real Mihomo for VLESS, HTTP, SOCKS5, SS2022 and Trojan', flush=True)
