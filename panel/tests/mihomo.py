"""Optional compatibility check using a separately verified official Mihomo binary."""
import http.client
import json
import os
import subprocess
import socket
import struct


def udp_echo(port, destination, marker=b'chain-protocol-udp-ok'):
    def receive(sock, size):
        data = b''
        while len(data) < size:
            chunk = sock.recv(size - len(data))
            assert chunk, 'SOCKS connection closed'
            data += chunk
        return data
    with socket.create_connection(('127.0.0.1', port), timeout=5) as control, socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as udp:
        udp.settimeout(5)
        control.sendall(b'\x05\x01\x00')
        assert receive(control, 2) == b'\x05\x00'
        control.sendall(b'\x05\x03\x00\x01' + b'\0' * 6)
        head = receive(control, 4)
        assert head[:2] == b'\x05\x00' and head[3] == 1, head
        address = socket.inet_ntoa(receive(control, 4))
        relay = struct.unpack('!H', receive(control, 2))[0]
        packet = b'\0\0\0\x01' + socket.inet_aton('127.0.0.1') + struct.pack('!H', destination) + marker
        udp.sendto(packet, ('127.0.0.1' if address == '0.0.0.0' else address, relay))
        response = udp.recv(65536)
        assert response == packet, f'UDP payload differs ({len(marker)} bytes sent, {len(response)-10} received)'


def exercise(h, lines, udp_port=None):
    vless = next(l for l in h.state()['lines'] if l['kind'] == 'vless' and l['access']['protocol'] == 'vless' and l['enabled'] and l['published'])
    for index, line in enumerate({l['id']: l for l in [vless, *lines]}.values(), start=len(h.processes)):
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
            if udp_port and node['type'] != 'http':
                udp_echo(port, udp_port)
            print(f"PASS Mihomo {line['name']}: exported {node['type']} TCP" + (' + UDP' if udp_port and node['type'] != 'http' else ''), flush=True)
        finally:
            h.stop_process(process)
    print('PASS original exported proxy YAML parsed and used by real Mihomo', flush=True)
