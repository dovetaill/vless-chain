"""Feature acceptance tests against the isolated, real Agent/Xray data plane."""
import base64
import concurrent.futures
import hashlib
import hmac
import http.client
import http.server
import json
import os
from pathlib import Path
import socket
import ssl
import struct
import time


def receive(sock, count):
    data = b""
    while len(data) < count:
        part = sock.recv(count - len(data))
        if not part:
            break
        data += part
    return data


def otp(secret, step):
    key = base64.b32decode(secret + "=" * (-len(secret) % 8))
    value = hmac.new(key, struct.pack("!Q", step), hashlib.sha1).digest()
    offset = value[-1] & 15
    return f"{(struct.unpack('!I', value[offset:offset + 4])[0] & 0x7fffffff) % 1000000:06d}"


class WebsiteBackend(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def do_GET(self):
        if self.headers.get("Upgrade", "").lower() == "websocket":
            accept = base64.b64encode(hashlib.sha1((self.headers['Sec-WebSocket-Key'] +
                '258EAFA5-E914-47DA-95CA-C5AB0DC85B11').encode()).digest()).decode()
            self.send_response(101)
            self.send_header("Connection", "Upgrade")
            self.send_header("Upgrade", "websocket")
            self.send_header("Sec-WebSocket-Accept", accept)
            self.end_headers()
            head = receive(self.connection, 2)
            if len(head) == 2:
                mask = receive(self.connection, 4)
                data = receive(self.connection, head[1] & 127)
                data = bytes(c ^ mask[i % 4] for i, c in enumerate(data))
                self.connection.sendall(bytes([0x81, len(data)]) + data)
            self.close_connection = True
            return
        data = self.path.encode()
        self.send_response(200)
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def log_message(self, *_):
        pass


def exercise(h, nodes, echo_port, agents, agent_envs):
    reality = next(line['access']['reality'] for line in h.state()['lines']
                   if line['access']['reality']['target'])
    old = h.request('/api/nodes', 'POST', {'name': '旧版本兼容检查', 'host': '127.0.0.1'})
    h.request(f"/api/agent/{old['id']}/sync", 'POST', {'running': True, 'applied': True, 'revision': '', 'error': ''},
              authenticated=False, headers={'Authorization': 'Bearer ' + old['token']})
    legacy = h.request('/api/lines', 'POST', {'name': '旧 Agent 保护', 'kind': 'vless',
                      'access': {'reality': reality},
                      'hops': [{'node_id': old['id'], 'port': h.port()}]})
    assert 'Agent' in h.request('/api/publish', 'POST', {}, status=400)['error']
    h.request('/api/lines/' + legacy['id'], 'DELETE')
    h.request('/api/nodes/' + old['id'], 'DELETE')
    print('PASS old-Agent capability check rejects incompatible publication before changing listeners', flush=True)
    # These Agents share one test host. Retire the completed multi-host fixtures
    # before reallocating internal listeners for the single-node feature checks.
    for line in h.state()['lines']:
        h.request('/api/lines/' + line['id'], 'DELETE')
    h.publish()
    def create(name, kind="vless", **extra):
        return h.request("/api/lines", "POST", {"name": name, "kind": kind,
            "hops": [{"node_id": nodes[0]["id"], "port": h.port()}], **extra})

    lines = [create(label, access={"protocol": protocol}) for protocol, label in
             [("http", "HTTP 代理"), ("socks", "SOCKS5 代理"), ("shadowsocks", "Shadowsocks 2022")]]
    tls = {"mode": "manual", "domain": "localhost", "cert": (h.root / "cert.pem").read_text(),
           "key": (h.root / "key.pem").read_text()}
    trojan = create("Trojan TLS", access={"protocol": "trojan", "tls": tls})
    vless_tls = create("VLESS TLS Vision", access={"protocol": "vless", "tls": tls})
    port = h.port()
    h.server(http.server.ThreadingHTTPServer(("127.0.0.1", port), WebsiteBackend))
    h.allow_loopback_target(port)
    site = create("演示网站", "website", target=f"http://127.0.0.1:{port}/app",
                  access={"tls": {"mode": "off", "domain": "app.example.test"}})
    other_site = h.request("/api/lines", "POST", {"name": "共享端口网站", "kind": "website",
        "hops": site["hops"], "target": f"http://127.0.0.1:{port}/other",
        "access": {"tls": {"mode": "off", "domain": "other.example.test"}}})
    secure_site = create("HTTPS 网站", "website", target=f"http://127.0.0.1:{port}", access={"tls": tls})
    limited = create("共享带宽验证", access={"protocol": "socks"},
                     limits={"upload_mbps": 0.8, "download_mbps": 0.8})
    plain = create("普通 SOCKS5 代理", access={"protocol": "socks"})
    quota = create("月额度验证", access={"protocol": "socks"},
                   limits={"monthly_gib": 8192 / 1073741824})
    occupied = next(n for n in h.state()['nodes'] if n['id'] == nodes[0]['id'])['report']['system']['listening_ports']
    automatic = h.request('/api/lines', 'POST', {'name': '自动端口分配', 'kind': 'vless',
        'access': {'protocol': 'socks'},
        'target': f'127.0.0.1:{echo_port}', 'hops': [{'node_id': nodes[0]['id']}]})
    allocated = automatic['hops'][0]['port']
    assert allocated not in occupied, 'Automatic port conflicts with an existing host listener'
    h.ports.add(allocated)
    h.record()
    h.publish()
    for line in lines:
        h.probe(line)
    h.probe(automatic)
    print("PASS real HTTP, authenticated SOCKS5 and Shadowsocks 2022 proxy connections", flush=True)
    h.probe(vless_tls)
    print("PASS real VLESS TLS + Vision with certificate verification", flush=True)

    context = ssl.create_default_context(cafile=str(h.root / "cert.pem"))
    with context.wrap_socket(socket.create_connection(("127.0.0.1", trojan["hops"][0]["port"]), timeout=8), server_hostname="localhost") as sock:
        password = trojan["access"]["password"]
        header = hashlib.sha224(password.encode()).hexdigest().encode() + b"\r\n\x01\x01" + socket.inet_aton("127.0.0.1") + struct.pack("!H", h.http_port) + b"\r\n"
        sock.sendall(header + b"GET / HTTP/1.0\r\nHost: localhost\r\n\r\n")
        data = b""
        while part := sock.recv(4096):
            data += part
        assert b"chain-integration-ok" in data, data
    print("PASS real Trojan over verified TLS", flush=True)

    def website(host, expected, status=200):
        client = http.client.HTTPConnection("127.0.0.1", site["hops"][0]["port"], timeout=8)
        try:
            client.request("GET", "/hello?q=1", headers={"Host": host})
            response = client.getresponse()
            assert response.status == status
            assert expected in response.read()
        finally:
            client.close()
    website("app.example.test", b"/app/hello?q=1")
    website("other.example.test", b"/other/hello?q=1")
    website("missing.example.test", "未配置".encode(), 404)
    client = http.client.HTTPSConnection("localhost", secure_site["hops"][0]["port"], context=context, timeout=8)
    try:
        client.request("GET", "/secure")
        assert client.getresponse().read() == b"/secure"
    finally:
        client.close()
    with socket.create_connection(("127.0.0.1", site["hops"][0]["port"]), timeout=8) as sock:
        sock.sendall(b"GET /socket HTTP/1.1\r\nHost: app.example.test\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n")
        header = b""
        while not header.endswith(b"\r\n\r\n"):
            header += receive(sock, 1)
            assert len(header) < 8192
        assert b"101" in header, header
        sock.sendall(b"\x81\x84\x01\x02\x03\x04" + bytes(c ^ [1, 2, 3, 4][i % 4] for i, c in enumerate(b"ping")))
        assert receive(sock, 6) == b"\x81\x04ping"
    print("PASS HTTP/HTTPS websites, same-port virtual hosts, paths and real WebSocket upgrade", flush=True)

    # Authenticate a SOCKS5 UDP association; its advertised port must be public.
    socks = lines[1]
    socks_port = socks["hops"][0]["port"]
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as udp:
        udp.settimeout(.4)
        packet = b"\x00\x00\x00\x01" + socket.inet_aton("127.0.0.1") + struct.pack("!H", echo_port) + b"udp-auth"
        try:
            udp.sendto(packet, ("127.0.0.1", socks_port))
            udp.recv(1024)
            raise AssertionError("Unauthenticated SOCKS UDP was accepted")
        except (socket.timeout, ConnectionRefusedError):
            pass
        with socket.create_connection(("127.0.0.1", socks_port), timeout=8) as sock:
            sock.sendall(b"\x05\x01\x02")
            assert receive(sock, 2) == b"\x05\x02"
            user, password = socks["access"]["username"].encode(), socks["access"]["password"].encode()
            sock.sendall(b"\x01" + bytes([len(user)]) + user + bytes([len(password)]) + password)
            assert receive(sock, 2) == b"\x01\x00"
            sock.sendall(b"\x05\x03\x00\x01\x00\x00\x00\x00\x00\x00")
            reply = receive(sock, 10)
            assert len(reply) == 10 and reply[:2] == b"\x05\x00" and reply[3] == 1, reply
            address = socket.inet_ntoa(reply[4:8])
            relay_port = struct.unpack("!H", reply[-2:])[0]
            assert relay_port > 0, reply
            relay = (sock.getpeername()[0] if address == "0.0.0.0" else address, relay_port)
            h.ports.add(relay_port)
            h.record()
            udp.settimeout(5)
            udp.sendto(packet, relay)
            assert udp.recv(1024).endswith(b"udp-auth")
        h.wait(lambda: all(int(entry.split()[1].split(':')[1], 16) != relay_port
            for table in ('udp', 'udp6') for entry in Path('/proc/net', table).read_text().splitlines()[1:]),
            "SOCKS UDP relay closure", timeout=5)
        udp.settimeout(.4)
        try:
            udp.sendto(packet, relay)
            udp.recv(1024)
            raise AssertionError("SOCKS UDP association remained authorized after close")
        except (socket.timeout, ConnectionRefusedError):
            pass
    print("PASS SOCKS UDP public endpoint, authentication and association lifetime", flush=True)

    def echo(line, count):
        with socket.create_connection(("127.0.0.1", line["hops"][0]["port"]), timeout=15) as sock:
            sock.sendall(b"\x05\x01\x02")
            assert receive(sock, 2) == b"\x05\x02"
            user, password = line["access"]["username"].encode(), line["access"]["password"].encode()
            sock.sendall(b"\x01" + bytes([len(user)]) + user + bytes([len(password)]) + password)
            assert receive(sock, 2) == b"\x01\x00"
            sock.sendall(b"\x05\x01\x00\x01" + socket.inet_aton("127.0.0.1") + struct.pack("!H", echo_port))
            assert receive(sock, 10)[:2] == b"\x05\x00"
            data = b"x" * count
            sock.sendall(data)
            assert receive(sock, count) == data
    started = time.monotonic()
    with concurrent.futures.ThreadPoolExecutor(max_workers=8) as pool:
        list(pool.map(lambda _: echo(limited, 25000), range(8)))
    elapsed = time.monotonic() - started
    assert 1.9 < elapsed < 8, elapsed
    started = time.monotonic()
    echo(plain, 200000)
    assert time.monotonic() - started < 1.5
    h.probe(limited)
    for _ in range(6):
        try:
            echo(quota, 1024)
        except (AssertionError, OSError):
            break
    def exhausted():
        line = next(l for l in h.state()["lines"] if l["id"] == quota["id"])
        return line.get("traffic", {}).get("upload", 0) + line.get("traffic", {}).get("download", 0) >= 8192
    h.wait(exhausted, "quota measurement")
    with socket.create_connection(("127.0.0.1", quota["hops"][0]["port"]), timeout=3) as sock:
        try:
            sock.sendall(b"blocked")
            assert sock.recv(100) == b""
        except ConnectionResetError:
            pass
    agent_dir = Path(agent_envs[0]["CHAIN_AGENT_DIR"])
    def xray_pid():
        return {p.name for p in Path('/proc').iterdir() if p.name.isdigit() and
                p.joinpath('cmdline').exists() and str(agent_dir).encode() in p.joinpath('cmdline').read_bytes() and b'xray' in p.joinpath('cmdline').read_bytes()}
    before = xray_pid()
    limited = h.request("/api/lines", "POST", {**limited, "limits": {"upload_mbps": 1.6, "download_mbps": 1.6}})
    assert h.request("/api/preview", "POST", {})["affected_line_ids"] == []
    h.publish()
    assert xray_pid() == before, "Limit-only changes restarted Xray"
    h.stop_process(agents[0])
    stopped_at = int(time.time())
    agents[0] = h.launch("agent-counter-restart", h.bin_dir / "chain-agent", agent_envs[0])
    h.wait(lambda: any(n["id"] == nodes[0]["id"] and n["seen"] > stopped_at and n["report"]["running"] for n in h.state()["nodes"]), "Agent restart")
    h.wait(exhausted, "persisted quota")
    h.probe(plain)
    print("PASS 8-connection aggregate bandwidth, unaffected line, TCP/UDP, quota persistence and live limit updates", flush=True)

    for line in lines + [trojan, vless_tls]:
        export = h.request(f"/api/lines/{line['id']}/export")
        assert export['share'] and export['node'].startswith('- ') and 'config' not in export
        node = json.loads(export['node'][2:])
        assert node['name'] == line['name'] and node['port'] == line['hops'][0]['port']
    sub = h.request('/api/subscriptions', 'POST', {'name': '测试订阅', 'line_ids': [l['id'] for l in lines]})
    assert b'proxies:' in h.request(sub['path'], authenticated=False)
    h.request('/api/subscriptions/' + sub['id'], 'DELETE')
    h.request(sub['path'], status=404, authenticated=False)
    exported = h.request(f"/api/lines/{lines[2]['id']}/export")
    external = h.request('/api/externals', 'POST', {'url': exported['node']})
    imported = h.request(f"/api/externals/{external['id']}/export")
    assert json.loads(imported['node'][2:])['type'] == 'ss'
    print("PASS exports, Mihomo import/export, subscription and revocation", flush=True)
    if os.environ.get('CHAIN_TEST_MIHOMO'):
        from mihomo import exercise as test_mihomo
        test_mihomo(h, lines + [trojan, vless_tls])

    token = (h.root / 'control/admin-token').read_text().strip()
    setup = h.request('/api/security/totp/start', 'POST', {'token': token})
    # Enroll with the prior valid step so normal login can use this step immediately.
    step = int(time.time()) // 30
    codes = h.request('/api/security/totp/finish', 'POST', {'code': otp(setup['secret'], step - 1)})['recovery_codes']
    h.request('/api/login', 'POST', {'token': token}, status=401, authenticated=False)
    h.request('/api/login', 'POST', {'token': token, 'code': otp(setup['secret'], step)}, authenticated=False)
    h.request('/api/login', 'POST', {'token': token, 'code': otp(setup['secret'], step)}, status=401, authenticated=False)
    h.request('/api/login', 'POST', {'token': token, 'code': codes[0]}, authenticated=False)
    h.request('/api/login', 'POST', {'token': token, 'code': codes[0]}, status=401, authenticated=False)
    h.request('/api/security/totp/disable', 'POST', {'token': token, 'code': codes[1]})
    h.login()
    new = h.request('/api/security/token', 'POST', {'token': token})['token']
    h.request('/api/state', status=401)
    h.request('/api/login', 'POST', {'token': token}, status=401, authenticated=False)
    assert (h.root / 'control/admin-token').read_text() == new
    h.login()
    assert h.state()['security']['totp_enabled'] is False
    print("PASS TOTP enrollment, replay rejection, single-use recovery, disable and administrator key rotation", flush=True)

    # Register internal loopback listeners for the harness cleanup audit too.
    for env in agent_envs:
        active = Path(env['CHAIN_AGENT_DIR']) / 'active.json'
        if active.exists():
            value = json.loads(active.read_text())
            for spec in value.get('config', value).get('_chain', []):
                h.ports.add(spec['backend'])
    h.record()
