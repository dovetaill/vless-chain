#!/usr/bin/env python3
"""Real controller/agent/Xray integration test; all listeners and files are task-owned.

CHAIN_TEST_BIN_DIR must contain chain-panel and chain-agent.
CHAIN_TEST_XRAY points to a verified Xray binary. No existing service is used.
"""
import contextlib
import http.cookiejar
import http.server
import json
import os
from pathlib import Path
import signal
import socket
import socketserver
import ssl
import subprocess
import tempfile
import threading
import time
import urllib.error
import urllib.request


class HttpHandler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        self.send_response(200)
        self.end_headers()
        self.wfile.write(b"chain-integration-ok")

    def log_message(self, *_):
        pass


class TcpEcho(socketserver.BaseRequestHandler):
    def handle(self):
        self.request.settimeout(5)
        with contextlib.suppress(OSError):
            while data := self.request.recv(4096):
                self.request.sendall(data)


class UdpEcho(socketserver.BaseRequestHandler):
    def handle(self):
        data, sock = self.request
        sock.sendto(data, self.client_address)


class TcpServer(socketserver.ThreadingTCPServer):
    daemon_threads = True


class TlsServer(http.server.ThreadingHTTPServer):
    def process_request_thread(self, request, client_address):
        # TLS negotiation must happen in each worker. Wrapping the listening
        # socket would serialize accept() behind incomplete REALITY probes.
        try:
            request.settimeout(5)
            secure = self.tls_context.wrap_socket(request, server_side=True)
        except (OSError, ssl.SSLError):
            request.close()
            return
        super().process_request_thread(secure, client_address)


class Harness:
    def __init__(self, root):
        self.root = root
        self.processes = []
        self.servers = []
        self.ports = set()
        self.bin_dir = Path(os.environ["CHAIN_TEST_BIN_DIR"])
        self.xray = Path(os.environ["CHAIN_TEST_XRAY"])
        self.cookie_jar = http.cookiejar.CookieJar()
        self.client = urllib.request.build_opener(urllib.request.HTTPCookieProcessor(self.cookie_jar))

    def record(self):
        (self.root / "ownership.json").write_text(json.dumps({
            "pid": os.getpid(), "pgid": os.getpgrp(), "cwd": str(Path.cwd()),
            "ports": sorted(self.ports), "groups": [p.pid for _, p, _ in self.processes],
            "files": str(self.root),
        }))

    def port(self):
        while True:
            with socket.socket() as s:
                s.bind(("127.0.0.1", 0))
                port = s.getsockname()[1]
            if port not in self.ports:
                self.ports.add(port)
                self.record()
                return port

    def launch(self, name, binary, env, args=None):
        log = (self.root / f"{name}.log").open("wb")
        merged = os.environ.copy()
        merged.update(env)
        p = subprocess.Popen([str(binary), *(args or [])], env=merged, stdin=subprocess.DEVNULL,
                             stdout=log, stderr=log, start_new_session=True)
        self.processes.append((name, p, log))
        self.record()
        return p

    def server(self, server):
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        self.servers.append((server, thread))
        return server

    def stop_process(self, p):
        with contextlib.suppress(ProcessLookupError):
            os.killpg(p.pid, signal.SIGTERM)
        try:
            p.wait(timeout=6)
        except subprocess.TimeoutExpired:
            os.killpg(p.pid, signal.SIGKILL)
            p.wait(timeout=5)

    def close(self):
        for _, p, log in reversed(self.processes):
            self.stop_process(p)
            log.close()
        for server, thread in self.servers:
            server.shutdown()
            server.server_close()
            thread.join(timeout=5)
        # Any remaining process group is a failed cleanup, not an ignored warning.
        for name, p, _ in self.processes:
            try:
                os.killpg(p.pid, 0)
            except ProcessLookupError:
                continue
            raise AssertionError(f"Owned process group still alive: {name} {p.pid}")
        for filename in ["tcp", "tcp6", "udp", "udp6"]:
            for entry in Path('/proc/net', filename).read_text().splitlines()[1:]:
                columns = entry.split()
                port = int(columns[1].split(':')[1], 16)
                if port in self.ports and (filename.startswith('udp') or columns[3] == '0A'):
                    raise AssertionError(f"Listener remains after cleanup: {filename} port {port}")
        print("CLEANUP groups=" + json.dumps([p.pid for _, p, _ in self.processes]) +
              " ports=" + json.dumps(sorted(self.ports)), flush=True)

    def request(self, path, method="GET", data=None, status=200, authenticated=True, headers=None):
        hdr = {"Content-Type": "application/json", "X-Chain-Request": "1"}
        hdr.update(headers or {})
        req = urllib.request.Request(self.base + path, method=method, headers=hdr,
                                     data=None if data is None else json.dumps(data).encode())
        client = self.client if authenticated else urllib.request.build_opener()
        try:
            response = client.open(req, timeout=48)
        except urllib.error.HTTPError as error:
            response = error
        body = response.read()
        assert response.status == status, (path, response.status, body.decode(errors="replace"))
        return json.loads(body) if path.startswith("/api/") else body

    def wait(self, callback, description, timeout=30):
        until = time.monotonic() + timeout
        while time.monotonic() < until:
            try:
                result = callback()
                if result:
                    return result
            except (ConnectionError, urllib.error.URLError, FileNotFoundError):
                pass
            time.sleep(.2)
        raise AssertionError("Timeout: " + description)

    def state(self):
        return self.request("/api/state")

    def login(self):
        token = (self.root / "control" / "admin-token").read_text()
        self.request("/api/login", "POST", {"token": token})

    def publish(self, expected="applied"):
        job = self.request("/api/publish", "POST", {})["job_id"]
        def done():
            j = next(j for j in self.state()["jobs"] if j["id"] == job)
            return j if j["state"] not in ("publishing", "rolling_back") else None
        result = self.wait(done, "publish job", timeout=60)
        assert result["state"] == expected, result
        return result

    def probe(self, line, mode="http", url=None):
        result = self.request(f"/api/lines/{line['id']}/probe", "POST", {
            "mode": mode, "url": url or f"http://127.0.0.1:{self.http_port}/"})
        assert result["ok"], result
        return result

    def browser(self):
        subprocess.run(["node", str(Path(__file__).with_name("browser.cjs")), self.base,
                        str(self.root / "control" / "admin-token"), str(self.root.parent), str(self.port())], check=True)

    def run(self):
        # A local TLS 1.3 target gives REALITY real handshakes without relying on public sites.
        cert, key = self.root / "cert.pem", self.root / "key.pem"
        subprocess.run(["openssl", "req", "-x509", "-newkey", "ec", "-pkeyopt",
                        "ec_paramgen_curve:prime256v1", "-nodes", "-keyout", str(key),
                        "-out", str(cert), "-days", "1", "-subj", "/CN=localhost", "-addext", "subjectAltName=DNS:localhost"],
                       check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        tls_port = self.port()
        tls_server = TlsServer(("127.0.0.1", tls_port), HttpHandler)
        tls = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        tls.minimum_version = ssl.TLSVersion.TLSv1_3
        # Do not advertise h2 for a server that only implements HTTP/1.x.
        # REALITY probes post-handshake records using the negotiated protocol.
        tls.set_alpn_protocols(["http/1.1"])
        tls.load_cert_chain(cert, key)
        tls_server.tls_context = tls
        self.server(tls_server)
        self.http_port = self.port()
        self.server(http.server.ThreadingHTTPServer(("127.0.0.1", self.http_port), HttpHandler))
        echo_port = self.port()
        self.server(TcpServer(("127.0.0.1", echo_port), TcpEcho))
        self.server(socketserver.ThreadingUDPServer(("127.0.0.1", echo_port), UdpEcho))
        control_port = self.port()
        self.base = f"http://127.0.0.1:{control_port}"
        control_env = {"CHAIN_DATA_DIR": str(self.root / "control"), "CHAIN_LISTEN": f"127.0.0.1:{control_port}",
                       "CHAIN_XRAY_BIN": str(self.xray), "CHAIN_APPLY_TIMEOUT": "9", "CHAIN_XRAY_LOG_LEVEL": "debug"}
        control = self.launch("control", self.bin_dir / "chain-panel", control_env)
        self.wait(lambda: self.request("/") or True, "controller start")
        self.request("/api/state", status=401, authenticated=False)
        self.request("/api/login", "POST", {"token": "wrong"}, status=401, authenticated=False)
        self.login()
        self.request("/api/preview", "POST", {}, status=403, headers={"Origin": "https://untrusted.example"})
        print("PASS authentication and cross-origin rejection", flush=True)

        nodes, agents, agent_envs = [], [], []
        for name in ["入口 A", "中转 B", "出口 C"]:
            n = self.request("/api/nodes", "POST", {"name": name, "host": "127.0.0.1",
                             "target": f"127.0.0.1:{tls_port}", "server_name": "localhost"})
            token_file = self.root / (n["id"] + ".token")
            token_file.write_text(n["token"])
            token_file.chmod(0o600)
            env = {"CHAIN_AGENT_DIR": str(self.root / n["id"]), "CHAIN_PANEL_URL": self.base,
                   "CHAIN_NODE_ID": n["id"], "CHAIN_NODE_TOKEN_FILE": str(token_file), "CHAIN_XRAY_BIN": str(self.xray)}
            agents.append(self.launch("agent-" + n["id"], self.bin_dir / "chain-agent", env))
            nodes.append(n); agent_envs.append(env)
        self.wait(lambda: all(n["online"] for n in self.state()["nodes"]), "agent registration")
        self.request(f"/api/agent/{nodes[1]['id']}/sync", "POST", {}, status=401,
                     authenticated=False, headers={"Authorization": "Bearer " + nodes[0]["token"]})
        assert "private_key" not in json.dumps(self.state())
        print("PASS node-scoped authentication and private-key redaction", flush=True)

        def line_input(name, path, kind="vless", **extra):
            return {"name": name, "kind": kind, "hops": [{"node_id": nodes[i]["id"], "port": self.port()} for i in path], **extra}
        main = self.request("/api/lines", "POST", line_input("三跳代理", [0, 1, 2]))
        shared = self.request("/api/lines", "POST", line_input("共享入口与出口", [0, 2]))
        forward = self.request("/api/lines", "POST", line_input("三跳 TCP / UDP", [0, 1, 2], "forward",
                               target=f"127.0.0.1:{echo_port}", network="tcp,udp"))
        bad = line_input("重复节点", [0, 0]); self.request("/api/lines", "POST", bad, status=400)
        bad = line_input("冲突端口", [0]); bad["hops"][0]["port"] = main["hops"][0]["port"]
        self.request("/api/lines", "POST", bad, status=400)
        assert len(self.state()["lines"]) == 3
        preview = self.request("/api/preview", "POST", {})
        assert len(preview["nodes"]) == 3
        self.publish()
        if os.environ.get("CHAIN_TEST_FEATURES_ONLY") == "1":
            from features import exercise
            exercise(self, nodes, echo_port, agents, agent_envs)
            if os.environ.get("CHAIN_TEST_BROWSER"):
                self.browser()
            return
        if os.environ.get("CHAIN_TEST_BROWSER_ONLY") == "1":
            self.browser()
            return
        self.probe(main); self.probe(shared)
        self.probe(forward, "tcp_echo"); self.probe(forward, "udp_echo")
        print("PASS simultaneous shared-node chains, real three-hop VLESS, TCP and UDP", flush=True)

        before = next(l for l in self.state()["lines"] if l["id"] == main["id"])["share"]
        edited = {**main, "hops": [main["hops"][0], main["hops"][2]]}
        main = self.request("/api/lines", "POST", edited)
        after = next(l for l in self.state()["lines"] if l["id"] == main["id"])["share"]
        assert before == after
        self.publish(); self.probe(main)
        print("PASS path changes preserve the entry share link", flush=True)

        external_link = next(l for l in self.state()["lines"] if l["id"] == shared["id"])["share"]
        external = self.request("/api/externals", "POST", {"name": "外部 VLESS 落地", "url": external_link})
        via_external = self.request("/api/lines", "POST", line_input("外部落地线路", [1], external_id=external["id"]))
        self.publish(); self.probe(via_external)
        print("PASS external VLESS import and real egress through imported link", flush=True)

        # The existing echo listener acts as an unrelated production listener: never kill it.
        bad = line_input("实际监听冲突", [1]); bad["hops"][0]["port"] = echo_port
        rejected = self.request("/api/lines", "POST", bad)
        self.publish("rolled_back")
        with socket.create_connection(("127.0.0.1", echo_port), timeout=3) as sock:
            sock.sendall(b"untouched"); assert sock.recv(9) == b"untouched"
        self.probe(via_external)
        self.request(f"/api/lines/{rejected['id']}", "DELETE")
        print("PASS occupied-port failure restores the prior config and preserves unrelated listener", flush=True)

        self.stop_process(control)
        control = self.launch("control-restart", self.bin_dir / "chain-panel", control_env)
        self.wait(lambda: self.request("/") or True, "controller restart")
        self.login(); assert len(self.state()["lines"]) == 4
        self.probe(main)
        print("PASS durable state and continued node traffic across controller restart", flush=True)

        # Interrupt a publish while the downstream Agent is offline. Recovery
        # must remain visibly incomplete until that Agent confirms restoration.
        self.stop_process(agents[2])
        interrupted = self.request("/api/lines", "POST", line_input("中断发布测试", [0, 2]))
        job_id = self.request("/api/publish", "POST", {})["job_id"]
        self.wait(lambda: any(n["id"] == nodes[2]["id"] and
                             (n["desired_revision"] or "").startswith(job_id)
                             for n in self.state()["nodes"]), "pending downstream config")
        self.stop_process(control)
        control = self.launch("control-recover", self.bin_dir / "chain-panel", control_env)
        self.wait(lambda: self.request("/") or True, "controller recovery start")
        self.login()
        self.wait(lambda: self.state()["jobs"][0]["state"] == "degraded", "offline recovery reported", timeout=45)
        agents[2] = self.launch("agent-reconnect", self.bin_dir / "chain-agent", agent_envs[2])
        self.wait(lambda: self.state()["jobs"][0]["state"] == "rolled_back", "late recovery acknowledgment", timeout=30)
        self.request(f"/api/lines/{interrupted['id']}", "DELETE")
        self.probe(main)
        print("PASS interrupted publish, offline recovery status, delayed Agent reconnection and restored traffic", flush=True)

        from features import exercise
        exercise(self, nodes, echo_port, agents, agent_envs)
        browser = os.environ.get("CHAIN_TEST_BROWSER")
        if browser:
            self.browser()
        print("ALL INTEGRATION CHECKS PASSED", flush=True)


if __name__ == "__main__":
    def interrupted(*_):
        raise KeyboardInterrupt()
    signal.signal(signal.SIGTERM, interrupted)
    with tempfile.TemporaryDirectory(prefix="chain-smoke-", dir=os.environ.get("CHAIN_TEST_ROOT")) as directory:
        harness = Harness(Path(directory))
        try:
            harness.run()
        except BaseException:
            for log in Path(directory).rglob("*.log"):
                print(f"LOG {log.name}:\n{log.read_text(errors='replace')[-1800:]}", flush=True)
            raise
        finally:
            harness.close()
