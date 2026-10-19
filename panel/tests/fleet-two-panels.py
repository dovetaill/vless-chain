"""Two-process API simulation; run as non-root in an owned temporary directory.

Arguments: chain-panel binary, work directory, certificate, key.
The isolated test build must trust the supplied localhost certificate in
fleet_api::request. Never install that test build or change system trust.
No Agent, core, maintenance operation or external notification is started.
"""
import http.client
import http.server
import json
import os
from pathlib import Path
import signal
import socket
import ssl
import subprocess
import sys
import threading
import time
import urllib.error
import urllib.request


def wait_for(check, seconds=40):
    end = time.monotonic() + seconds
    while time.monotonic() < end:
        if check():
            return
        time.sleep(1)
    raise AssertionError("Timed out waiting for expected state")


def free_port():
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


class Panel:
    def __init__(self, name):
        self.dir = work / name
        self.dir.mkdir()
        self.port = free_port()
        self.cookie = ""
        self.process = None
        self.log = (work / f"{name}.log").open("wb")
        try:
            self.start()
        except BaseException:
            self.stop()
            self.log.close()
            raise

    def start(self):
        self.process = subprocess.Popen(
            [binary], cwd=self.dir, start_new_session=True,
            env={**os.environ, "CHAIN_DATA_DIR": str(self.dir),
                 "CHAIN_LISTEN": f"127.0.0.1:{self.port}",
                 "CHAIN_SECURE_COOKIE": "1", "CHAIN_XRAY_BIN": "/nonexistent/test-xray"},
            stdout=self.log, stderr=self.log)
        print(f"PROCESS {self.dir.name} pid/pgid={self.process.pid} port={self.port}", flush=True)
        wait_for(lambda: self.ready(), 10)

    def ready(self):
        if self.process.poll() is not None:
            raise AssertionError(f"Panel exited; inspect {work / (self.dir.name + '.log')}")
        try:
            with socket.create_connection(("127.0.0.1", self.port), timeout=.2):
                return (self.dir / "admin-token").exists()
        except OSError:
            return False

    def stop(self):
        if self.process and self.process.poll() is None:
            os.killpg(self.process.pid, signal.SIGTERM)
            try:
                self.process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                os.killpg(self.process.pid, signal.SIGKILL)
                self.process.wait(timeout=5)

    def api(self, path, body=None, method=None, cookie=None, expected=200):
        request = urllib.request.Request(
            f"http://127.0.0.1:{self.port}/api{path}",
            data=None if body is None else json.dumps(body).encode(),
            method=method or ("GET" if body is None else "POST"),
            headers={"Content-Type": "application/json", "X-Chain-Request": "1",
                     "Cookie": self.cookie if cookie is None else cookie})
        try:
            response = urllib.request.urlopen(request, timeout=20)
        except urllib.error.HTTPError as e:
            response = e
        with response:
            value = json.load(response)
            assert response.status == expected, (path, response.status, value)
            return value, response.headers.get("Set-Cookie", "").split(";")[0]

    def login(self):
        _, self.cookie = self.api("/login", {"token": (self.dir / "admin-token").read_text().strip()})


if __name__ == "__main__":
    assert os.geteuid() != 0, "Run as an unprivileged user"
    binary, directory, cert, key = sys.argv[1:]
    work = Path(directory).resolve()
    work.mkdir()
    panels = []
    server = thread = None
    try:
        master = Panel("master")
        panels.append(master)
        slave = Panel("slave")
        panels.append(slave)

        class Proxy(http.server.BaseHTTPRequestHandler):
            def log_message(self, *args):
                pass

            def forward(self):
                connection = http.client.HTTPConnection("127.0.0.1", slave.port, timeout=20)
                try:
                    body = self.rfile.read(int(self.headers.get("Content-Length", 0)))
                    connection.request(self.command, self.path, body, dict(self.headers))
                    response = connection.getresponse()
                    data = response.read()
                    self.send_response(response.status)
                    self.send_header("Content-Type", "application/json")
                    self.send_header("Content-Length", str(len(data)))
                    self.end_headers()
                    self.wfile.write(data)
                except OSError:
                    self.send_error(502)
                finally:
                    connection.close()

            do_GET = do_POST = do_DELETE = forward

        server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Proxy)
        context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        context.load_cert_chain(cert, key)
        server.socket = context.wrap_socket(server.socket, server_side=True)
        thread = threading.Thread(target=server.serve_forever)
        thread.start()
        print(f"HTTPS port={server.server_port}", flush=True)
        master.login()
        slave.login()
        code = slave.api("/fleet/invite", {})[0]["code"]
        remote = master.api("/fleet/connect", {"name": "sim-slave", "code": code,
                           "url": f"https://localhost:{server.server_port}"})[0]["id"]

        def forward(path, body=None, method=None, expected=200):
            return master.api(f"/fleet/{remote}/forward", {"path": path, "body": body,
                              "method": method or ("GET" if body is None else "POST")}, expected=expected)[0]

        slave.api("/federation/pair", {"code": code, "name": "replay",
                  "controller_id": "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"}, cookie="", expected=400)
        forward("/security/token", {}, expected=403)
        external = forward("/externals", {"name": "test", "url": "socks5://127.0.0.1:10999"})
        forward(f"/externals/{external['id']}", {"name": "updated", "url": "socks5://127.0.0.1:10998"})
        assert slave.api("/state")[0]["externals"][0]["name"] == "updated"
        assert master.api("/state")[0]["externals"] == []
        node = slave.api("/nodes", {"name": "sim-agent", "host": "127.0.0.1"})[0]
        line_input = {"name": "sim-vless", "kind": "vless", "hops": [{"node_id": node["id"]}]}
        line = forward("/lines", line_input)
        forward("/lines", {**line_input, "id": line["id"], "name": "edited-vless"})
        assert forward("/state")["lines"][0]["name"] == "edited-vless"
        forward(f"/lines/{line['id']}", method="DELETE")
        forward(f"/externals/{external['id']}", method="DELETE")
        slave.api(f"/nodes/{node['id']}", method="DELETE")
        print("PASS HTTPS pairing, replay rejection, remote CRUD, data isolation, route boundary", flush=True)

        local = slave.api("/monitoring")[0]
        policy = {**local["local"], "enabled": True, "interval": 60, "offline": 30, "recoveries": 1}
        master.api("/fleet/policy", policy)
        wait_for(lambda: slave.api("/monitoring")[0]["effective"] == policy)
        assert slave.api("/monitoring")[0]["local"] == local["local"]
        wait_for(lambda: bool(master.api("/fleet")[0]["remotes"][0]["snapshot"]))
        print("PASS scheduled aggregation and policy override preserving local settings", flush=True)

        def delegate():
            url = master.api(f"/fleet/{remote}/login", {})[0]["url"]
            ticket = urllib.parse.unquote(url.split("fleet-ticket=")[1])
            _, cookie = slave.api("/federation/login", {"ticket": ticket}, cookie="")
            slave.api("/federation/login", {"ticket": ticket}, cookie="", expected=401)
            slave.api("/state", cookie=cookie)
            return cookie

        delegate()
        slave.stop()
        wait_for(lambda: master.api("/monitoring")[0]["conditions"].get(f"panel:{remote}", {}).get("active"), 80)
        notices = master.api("/monitoring")[0]["notices"]
        assert len(notices) == 1, notices
        print("PASS real remote shutdown detected and one failure notice queued", flush=True)
        slave.start()
        slave.login()
        assert slave.api("/monitoring")[0]["effective"] == policy
        wait_for(lambda: not master.api("/monitoring")[0]["conditions"][f"panel:{remote}"]["active"])
        assert len(master.api("/monitoring")[0]["notices"]) == 2
        print("PASS restart persisted pairing/policy, reconnect and recovery notice", flush=True)
        master.stop()
        slave.api("/state")
        assert slave.api("/monitoring")[0]["effective"] == policy
        master.start()
        master.login()
        assert master.api("/fleet")[0]["remotes"][0]["id"] == remote
        delegated = delegate()
        master.api(f"/fleet/{remote}", method="DELETE")
        assert slave.api("/monitoring")[0]["effective"] == local["local"]
        slave.api("/state", cookie=delegated, expected=401)
        slave.api("/state")
        assert master.api("/fleet")[0]["remotes"] == []
        print("PASS controller outage independence, restart persistence, unbind policy/session revocation", flush=True)
    finally:
        if server:
            server.shutdown()
            server.server_close()
            thread.join(timeout=5)
        for panel in panels:
            panel.stop()
            panel.log.close()
            assert panel.process.poll() is not None
            with socket.socket() as s:
                assert s.connect_ex(("127.0.0.1", panel.port)) != 0
        print("CLEANUP panel process groups stopped and panel ports released", flush=True)
