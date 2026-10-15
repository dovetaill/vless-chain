#!/usr/bin/env python3
"""Focused rental acceptance against task-owned controller, Agent and real Xray.

Requires CHAIN_TEST_ROOT and CHAIN_TEST_BIN_DIR. CHAIN_TEST_XRAY may override the
installed core. No deployment, existing service, build directory or cache is changed.
"""
import contextlib
import json
import os
from pathlib import Path
import signal
import socket
import socketserver
import struct
import sys
import tempfile

sys.dont_write_bytecode = True
from features import receive
from smoke import Harness, TcpServer, UdpEcho


class RentalEcho(socketserver.BaseRequestHandler):
    def handle(self):
        # Keep idle connections alive long enough to prove policy-driven termination.
        self.request.settimeout(60)
        with contextlib.suppress(OSError):
            while data := self.request.recv(65536):
                self.request.sendall(data)


@contextlib.contextmanager
def socks(line, target_port, command=1, password=None):
    with socket.create_connection(("127.0.0.1", line["hops"][0]["port"]), timeout=5) as connection:
        connection.sendall(b"\x05\x01\x02")
        assert receive(connection, 2) == b"\x05\x02"
        user = line["access"]["username"].encode()
        secret = (password if password is not None else line["access"]["password"]).encode()
        connection.sendall(b"\x01" + bytes([len(user)]) + user + bytes([len(secret)]) + secret)
        try:
            auth = receive(connection, 2)
        except ConnectionResetError:
            if password is None:
                raise
            auth = b""
        if password is not None:
            assert auth != b"\x01\x00", "Wrong SOCKS credentials were accepted"
            yield connection
            return
        assert auth == b"\x01\x00", auth
        connection.sendall(bytes([5, command, 0, 1]) + socket.inet_aton("127.0.0.1") + struct.pack("!H", target_port))
        reply = receive(connection, 10)
        assert reply[:2] == b"\x05\x00", reply
        if command == 3:
            assert len(reply) == 10 and reply[3] == 1, reply
            address = socket.inet_ntoa(reply[4:8])
            port = struct.unpack("!H", reply[-2:])[0]
            assert port > 0, reply
            yield (connection.getpeername()[0] if address == "0.0.0.0" else address, port)
        else:
            yield connection


def echo(line, target_port, data=b"rental-tcp-payload"):
    with socks(line, target_port) as connection:
        connection.sendall(data)
        assert receive(connection, len(data)) == data


def remember_internal_ports(harness, agent_dir):
    active = agent_dir / "active.json"
    if active.exists():
        value = json.loads(active.read_text())
        config = value.get("config", value)
        for inbound in config.get("inbounds", []):
            harness.ports.add(inbound["port"])
        for listener in config.get("_mihomo", {}).get("listeners", []):
            harness.ports.add(listener["port"])
        for spec in config.get("_chain", []):
            for key in ("port", "backend", "fallback_port"):
                if isinstance(spec.get(key), int) and spec[key] > 0:
                    harness.ports.add(spec[key])
            for key in ("port", "backend"):
                if spec.get("meter") and spec["meter"].get(key):
                    harness.ports.add(spec["meter"][key])
        harness.record()


def exercise(h):
    echo_port = h.port()
    h.server(TcpServer(("127.0.0.1", echo_port), RentalEcho))
    h.server(socketserver.ThreadingUDPServer(("127.0.0.1", echo_port), UdpEcho))
    control_port = h.port()
    h.base = f"http://127.0.0.1:{control_port}"
    h.launch("control", h.bin_dir / "chain-panel", {
        "CHAIN_DATA_DIR": str(h.root / "control"), "CHAIN_LISTEN": f"127.0.0.1:{control_port}",
        "CHAIN_XRAY_BIN": str(h.xray), "CHAIN_APPLY_TIMEOUT": "9",
        "CHAIN_XRAY_LOG_LEVEL": "warning",
    })
    h.wait(lambda: (h.root / "control" / "admin-token").exists() and h.request("/") or False, "controller start")
    h.login()
    h.request("/api/state", status=401, authenticated=False)
    server = h.request("/api/nodes", "POST", {"name": "租赁验证节点", "host": "127.0.0.1",
        "target": f"127.0.0.1:{echo_port}", "server_name": "localhost"})
    token_file = h.root / "node.token"
    token_file.write_text(server["token"])
    token_file.chmod(0o600)
    agent_dir = h.root / "agent"
    agent_env = {"CHAIN_AGENT_DIR": str(agent_dir), "CHAIN_PANEL_URL": h.base,
        "CHAIN_NODE_ID": server["id"], "CHAIN_NODE_TOKEN_FILE": str(token_file), "CHAIN_XRAY_BIN": str(h.xray)}
    agent = h.launch("agent", h.bin_dir / "chain-agent", agent_env)
    h.wait(lambda: any(n["id"] == server["id"] and n["online"] for n in h.state()["nodes"]), "agent registration")

    def current(line):
        return next(item for item in h.state()["lines"] if item["id"] == line["id"])

    def synced(line):
        h.wait(lambda: current(line)["sync_status"] == "synced", "rental policy acknowledgement")
        remember_internal_ports(h, agent_dir)

    def public(config=None, token=None, status=200):
        return h.request("/api/public/lookup", "POST", {"config": config} if config is not None else {"token": token},
            status=status, authenticated=False)

    def operate(lines, action, **extra):
        return h.request("/api/lines/operate", "POST", {"line_ids": [line["id"] for line in lines], "action": action, **extra})

    def traffic(line):
        return current(line)["traffic"] or {}

    def used(line):
        value = traffic(line)
        return value.get("upload", 0) + value.get("download", 0)

    lines = [h.request("/api/lines", "POST", {"name": name, "kind": "vless",
        "hops": [{"node_id": server["id"], "port": h.port()}], "access": {"protocol": "socks"},
        "limits": {"monthly_gib": 8192 / 1073741824} if index == 2 else {}})
        for index, name in enumerate(["主租赁", "同客户另一代理", "硬额度租赁"])]
    main, companion, quota = lines
    h.publish()
    for line in lines:
        synced(line)
    from rentals_protocols import exercise as protocol_metering
    protocol_metering(h, server, echo_port, agent_dir)
    exports = [h.request(f"/api/lines/{line['id']}/export") for line in lines]
    nodes = [json.loads(value["node"][2:]) for value in exports]
    public(exports[0]["share"], status=404)

    temporary = h.request("/api/customers", "POST", {"name": "临时客户"})
    h.request("/api/customers", "POST", {**temporary, "name": "改名客户", "contact": "private-temporary"})
    assert next(c for c in h.state()["customers"] if c["id"] == temporary["id"])["name"] == "改名客户"
    h.request(f"/api/customers/{temporary['id']}", "DELETE")
    assert all(c["id"] != temporary["id"] for c in h.state()["customers"])
    customer = h.request("/api/customers", "POST", {"name": "private-customer-name",
        "contact": "private-contact", "notes": "private-notes"})
    prices = {"monthly": 1200, "quarterly": 3200, "yearly": 10000}
    h.request("/api/rentals/contact", "POST", {"renewal_contact": "运营续费入口"})
    operate([quota], "unlimited")
    synced(quota)
    personal = current(quota)
    assert personal["rental"]["customer_id"] == ""
    assert personal["rental"]["started_at"] > 0 and personal["rental"]["expires_at"] == 0
    assert personal["enabled"] is True and personal["limits"] == quota["limits"]
    assert not h.state()["payments"]
    assert public(exports[2]["share"])["lines"][0]["rental"]["permanent"] is True
    rented = [main, companion]
    for line in rented:
        h.request(f"/api/rentals/{line['id']}", "POST", {"customer_id": customer["id"], "prices": prices})
        synced(line)
        saved = current(line)
        assert saved["rental"]["prices"] == prices
        assert saved["rental"]["started_at"] == saved["rental"]["expires_at"] == 0
        assert saved["rental_status"] == "not_started"
    h.request(f"/api/customers/{customer['id']}", "DELETE", status=400)
    with socket.create_connection(("127.0.0.1", main["hops"][0]["port"]), timeout=3) as connection:
        with contextlib.suppress(ConnectionResetError, BrokenPipeError):
            connection.sendall(b"\x05\x01\x02")
            assert connection.recv(2) == b"", "Unopened rental accepted a connection"
    print("PASS customer CRUD, prices and unopened rental blocking", flush=True)

    opening = {"line_ids": [line["id"] for line in rented], "term": "monthly", "amounts": {main["id"]: 900},
        "note": "人工开通", "request_id": "rental-opening"}
    opened = h.request("/api/rentals/renew", "POST", opening)
    again = h.request("/api/rentals/renew", "POST", opening)
    assert opened["payments"] == again["payments"]
    assert len(h.state()["payments"]) == 2
    h.request("/api/rentals/renew", "POST", {**opening, "note": "不能复用为另一笔订单"}, status=400)
    for line in lines:
        synced(line)
        assert current(line)["rental_status"] == "active"

    baseline = used(main)
    sampled = traffic(main).get("sampled_at", 0)
    with socks(main, echo_port, password="wrong-rental-secret"):
        pass
    packet = b"\x00\x00\x00\x01" + socket.inet_aton("127.0.0.1") + struct.pack("!H", echo_port) + b"unauthorized"
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as udp:
        udp.settimeout(.4)
        try:
            udp.sendto(packet, ("127.0.0.1", main["hops"][0]["port"]))
            udp.recv(1024)
            raise AssertionError("Unauthenticated UDP was accepted")
        except (socket.timeout, ConnectionRefusedError):
            pass
    h.wait(lambda: traffic(main).get("sampled_at", 0) > sampled, "fresh counter sample after rejected credentials")
    assert used(main) == baseline, "Rejected SOCKS traffic consumed the allowance"
    payload = b"rental-tcp-" * 512
    echo(main, echo_port, payload)
    h.wait(lambda: used(main) >= baseline + 2 * len(payload), "authenticated TCP accounting")
    before_udp = used(main)
    udp_payload = b"rental-authenticated-udp"
    packet = packet[:10] + udp_payload

    def rejected_udp(client, destination, message):
        client.settimeout(.4)
        try:
            client.sendto(packet, destination)
            client.recv(1024)
            raise AssertionError(message)
        except (socket.timeout, ConnectionRefusedError):
            pass

    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as udp, socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as other:
        for client in (udp, other):
            client.bind(("127.0.0.1", 0))
            h.ports.add(client.getsockname()[1])
        h.record()
        with socks(main, udp.getsockname()[1], command=3) as relay:
            h.ports.add(relay[1])
            h.record()
            udp.settimeout(5)
            udp.sendto(packet, relay)
            assert udp.recv(1024).endswith(udp_payload)
            h.wait(lambda: used(main) >= before_udp + 2 * len(udp_payload), "authenticated UDP accounting")
            authorized = traffic(main)
            authorized_bytes = authorized["upload"] + authorized["download"]
            with socks(main, echo_port, password="wrong-rental-secret"):
                pass
            rejected_udp(other, ("127.0.0.1", main["hops"][0]["port"]), "Same-IP unauthenticated UDP borrowed A's association")
            rejected_udp(other, relay, "Same-IP different-source-port UDP borrowed A's relay")
            h.wait(lambda: traffic(main).get("sampled_at", 0) > authorized["sampled_at"], "counter sample after same-IP rejected UDP")
            assert used(main) == authorized_bytes, "Another client's rejected UDP consumed A's allowance"
            udp.sendto(packet, relay)
            assert udp.recv(1024).endswith(udp_payload), "Rejected client disrupted A's valid association"
            h.wait(lambda: used(main) >= authorized_bytes + 2 * len(udp_payload), "A remains authorized after B is rejected")
            before_close = traffic(main)

        def relay_closed():
            return all(int(entry.split()[1].split(":")[1], 16) != relay[1]
                for table in ("udp", "udp6") for entry in Path("/proc/net", table).read_text().splitlines()[1:])

        h.wait(relay_closed, "UDP relay closes with its authenticated TCP control", timeout=5)
        rejected_udp(udp, relay, "Closed SOCKS control left its UDP relay authorized")
        h.wait(lambda: traffic(main).get("sampled_at", 0) > before_close["sampled_at"], "counter sample after closed UDP association")
        assert used(main) == before_close["upload"] + before_close["download"], "Closed UDP association continued charging"
    print("PASS real SOCKS TCP/UDP accounting, same-IP UDP isolation and relay closure", flush=True)

    before_renewal = current(main)
    h.request(f"/api/rentals/{main['id']}", "POST", {"customer_id": customer["id"], "prices": {**prices, "monthly": 1800}})
    renewal = {"line_ids": [main["id"]], "term": "monthly", "request_id": "rental-renewal"}
    paid = h.request("/api/rentals/renew", "POST", renewal)["payments"][0]
    synced(main)
    renewed = current(main)
    assert renewed["rental"]["started_at"] == before_renewal["rental"]["started_at"]
    assert renewed["rental"]["expires_at"] > before_renewal["rental"]["expires_at"]
    assert renewed["traffic"]["cycle_start"] == before_renewal["traffic"]["cycle_start"]
    assert renewed["traffic"]["upload"] + renewed["traffic"]["download"] == before_renewal["traffic"]["upload"] + before_renewal["traffic"]["download"]
    payments = [p for p in h.state()["payments"] if p["line_id"] == main["id"]]
    assert [p["amount_fen"] for p in payments] == [900, 1800]
    assert paid["previous_expires_at"] == before_renewal["rental"]["expires_at"]
    print("PASS idempotent manual opening, historical amounts and renewal without resetting usage", flush=True)

    wrong = {**nodes[0], "password": "incorrect"}
    public(json.dumps(wrong), status=404)
    public(json.dumps({"proxy-providers": {"remote": {"url": "http://127.0.0.1:1/never-fetch"}}}), status=404)
    result = public(json.dumps({"proxies": [nodes[0], {**nodes[1], "password": "incorrect"}]}))
    assert [line["id"] for line in result["lines"]] == [main["id"]]
    assert result["unmatched"] == ["第 2 个代理"]
    assert result["renewal_contact"] == "运营续费入口"
    serialized = json.dumps(result)
    for forbidden in [customer["id"], "private-customer-name", "private-contact", "private-notes", "token_hash", "password"]:
        assert forbidden not in serialized
    result = public(json.dumps([nodes[0], nodes[1]]))
    assert {line["id"] for line in result["lines"]} == {main["id"], companion["id"]}
    old_path = h.request(f"/api/rentals/{main['id']}/query-link", "POST", {})["path"]
    token = old_path.split("#token=", 1)[1]
    assert [line["id"] for line in public(token=token)["lines"]] == [main["id"]]
    new_path = h.request(f"/api/rentals/{main['id']}/query-link", "POST", {})["path"]
    public(token=token, status=404)
    token = new_path.split("#token=", 1)[1]
    print("PASS public credential isolation, provider rejection and query-token rotation", flush=True)

    draft_port = h.port()
    h.request("/api/lines", "POST", {**companion, "name": "未发布的另一代理草稿",
        "hops": [{**companion["hops"][0], "port": draft_port}]})
    with socks(main, echo_port) as existing:
        existing.sendall(b"before-disable")
        assert receive(existing, 14) == b"before-disable"
        operate([main], "disable")
        synced(main)
        existing.settimeout(3)
        try:
            assert existing.recv(1) == b"", "Disabled rental kept an existing stream alive"
        except ConnectionResetError:
            pass
    assert public(token=token)["lines"][0]["status"] == "disabled"
    h.request("/api/rentals/renew", "POST", {"line_ids": [main["id"]], "term": "monthly", "request_id": "disabled-renewal"})
    synced(main)
    assert current(main)["enabled"] is False
    assert public(exports[0]["share"])["lines"][0]["status"] == "disabled"
    before_unlimited = current(main)["rental"]
    payments_before = len(h.state()["payments"])
    operate([main], "unlimited")
    synced(main)
    unlimited = current(main)
    assert unlimited["enabled"] is False
    assert unlimited["rental"]["expires_at"] == 0
    for field in ["customer_id", "prices", "started_at"]:
        assert unlimited["rental"][field] == before_unlimited[field]
    assert len(h.state()["payments"]) == payments_before
    public_unlimited = public(token=token)["lines"][0]
    assert public_unlimited["rental"]["permanent"] is True and public_unlimited["status"] == "disabled"
    with socket.create_connection(("127.0.0.1", main["hops"][0]["port"]), timeout=3) as connection:
        with contextlib.suppress(ConnectionResetError, BrokenPipeError):
            connection.sendall(b"\x05\x01\x02")
            assert connection.recv(2) == b"", "Unlimited mode bypassed manual disable"
    active = json.loads((agent_dir / "active.json").read_text())["config"]
    deployed = next(spec for spec in active["_chain"] if spec["id"] == companion["id"])
    assert deployed["port"] == companion["hops"][0]["port"]
    assert all(spec["port"] != draft_port for spec in active["_chain"])
    assert current(companion)["published"] is False
    echo(companion, echo_port)
    operate([main], "enable")
    synced(main)
    echo(main, echo_port)
    print("PASS disabling closes active streams; renewal preserves disable; other saved drafts stay unpublished", flush=True)

    with socks(quota, echo_port) as connection:
        connection.sendall(b"q" * 16384)
        returned = b""
        try:
            while part := connection.recv(16384):
                returned += part
        except ConnectionResetError:
            pass
        assert len(returned) < 16384
    h.wait(lambda: traffic(quota).get("blocked") and used(quota) == 8192, "exact hard quota exhaustion")
    assert current(quota)["rental_status"] == "quota_exhausted"
    assert public(exports[2]["share"])["lines"][0]["rental"]["permanent"] is True
    with socket.create_connection(("127.0.0.1", quota["hops"][0]["port"]), timeout=3) as connection:
        with contextlib.suppress(ConnectionResetError, BrokenPipeError):
            connection.sendall(b"\x05\x01\x02")
            assert connection.recv(2) == b"", "Exhausted rental accepted a connection"
    print("PASS unlimited self-use retains the 8192-byte hard quota without creating payments", flush=True)

    h.stop_process(agent)
    h.wait(lambda: not next(n for n in h.state()["nodes"] if n["id"] == server["id"])["online"], "offline node status", timeout=22)
    operation = operate([main], "limits", limits={"upload_mbps": 20, "download_mbps": 20})
    assert operation["results"][0]["sync_status"] == "pending"
    state = public(token=token)["lines"][0]
    assert state["status"] == "offline" and state["sync_status"] == "pending"
    h.launch("agent-reconnected", h.bin_dir / "chain-agent", agent_env)
    synced(main)
    echo(main, echo_port)
    assert public(token=token)["lines"][0]["status"] == "active"
    print("PASS offline operations remain pending until the real Agent acknowledges", flush=True)

    operate([main], "delete")
    public(token=token, status=404)
    public(exports[0]["share"], status=404)
    assert any(p["line_id"] == main["id"] and p["amount_fen"] == 900 for p in h.state()["payments"])
    operate([companion, quota], "delete")
    h.request(f"/api/customers/{customer['id']}", "DELETE")
    assert not h.state()["lines"] and not h.state()["customers"]
    print("PASS deletion revokes lookup and preserves payment history", flush=True)
    print("ALL RENTAL ACCEPTANCE CHECKS PASSED", flush=True)


def main():
    os.environ.setdefault("CHAIN_TEST_XRAY", "/opt/chain/releases/0.2.6-linux-x86_64-329ef722a8b6/bin/xray")
    def interrupted(*_):
        raise KeyboardInterrupt()
    signal.signal(signal.SIGTERM, interrupted)
    directory = None
    try:
        with tempfile.TemporaryDirectory(prefix="chain-rentals-", dir=os.environ["CHAIN_TEST_ROOT"]) as directory:
            harness = Harness(Path(directory))
            try:
                exercise(harness)
            except BaseException:
                for log in Path(directory).glob("*.log"):
                    print(f"LOG {log.name}:\n{log.read_text(errors='replace')[-1800:]}", flush=True)
                raise
            finally:
                try:
                    remember_internal_ports(harness, Path(directory) / "agent")
                finally:
                    harness.close()
    finally:
        if directory is not None:
            assert not Path(directory).exists(), f"Temporary artifacts remain: {directory}"
            print(f"CLEANUP artifacts removed: {directory}; preserved pre-existing binaries and services", flush=True)


if __name__ == "__main__":
    main()
