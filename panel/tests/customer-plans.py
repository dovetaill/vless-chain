#!/usr/bin/env python3
"""Real customer-plan acceptance; requires task-owned ROOT, BIN_DIR and XRAY.

Run as an unprivileged user with CHAIN_TEST_ROOT, CHAIN_TEST_BIN_DIR and
CHAIN_TEST_XRAY set. All controllers, Agents, cores, listeners and data are owned
by this invocation and are removed in finally; no installed service is contacted.
"""
import contextlib
import http.cookiejar
import json
import os
from pathlib import Path
import signal
import socket
import sqlite3
import ssl
import subprocess
import sys
import tempfile
import time
import urllib.request

sys.dont_write_bytecode = True
from rentals import RentalEcho, echo, remember_internal_ports
from smoke import Harness, HttpHandler, TcpServer, TlsServer


def database(path, query):
    with contextlib.closing(sqlite3.connect(path.resolve().as_uri() + "?mode=ro", uri=True)) as db:
        return db.execute(query).fetchall()


@contextlib.contextmanager
def login_as(h, token):
    previous = h.client
    jar = http.cookiejar.CookieJar()
    h.client = urllib.request.build_opener(urllib.request.HTTPCookieProcessor(jar))
    try:
        h.request("/api/login", "POST", {"token": token})
        yield h.client
    finally:
        h.client = previous


def exercise(h):
    echo_port = h.port()
    h.server(TcpServer(("127.0.0.1", echo_port), RentalEcho))
    cert, key = h.root / "cert.pem", h.root / "key.pem"
    subprocess.run(["openssl", "req", "-x509", "-newkey", "ec", "-pkeyopt",
        "ec_paramgen_curve:prime256v1", "-nodes", "-keyout", str(key), "-out", str(cert),
        "-days", "1", "-subj", "/CN=localhost", "-addext", "subjectAltName=DNS:localhost"],
        check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    tls_port = h.port()
    tls_server = TlsServer(("127.0.0.1", tls_port), HttpHandler)
    tls = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    tls.minimum_version = ssl.TLSVersion.TLSv1_3
    tls.set_alpn_protocols(["http/1.1"])
    tls.load_cert_chain(cert, key)
    tls_server.tls_context = tls
    h.server(tls_server)

    control_port = h.port()
    h.base = f"http://127.0.0.1:{control_port}"
    control_env = {"CHAIN_DATA_DIR": str(h.root / "control"),
        "CHAIN_LISTEN": f"127.0.0.1:{control_port}", "CHAIN_XRAY_BIN": str(h.xray),
        "CHAIN_APPLY_TIMEOUT": "15", "CHAIN_XRAY_LOG_LEVEL": "warning", "SSL_CERT_FILE": str(cert)}
    control = h.launch("control", h.bin_dir / "chain-panel", control_env)
    h.wait(lambda: (h.root / "control" / "admin-token").exists() and h.request("/") or False, "controller start")
    h.login()
    admin_token = (h.root / "control" / "admin-token").read_text()
    nodes, agents, agent_envs = [], [], []
    for index in range(2):
        node = h.request("/api/nodes", "POST", {"name": f"套餐入口 {index}", "host": "127.0.0.1",
            "target": f"127.0.0.1:{tls_port}", "server_name": "localhost"})
        token_file = h.root / f"node-{index}.token"
        with os.fdopen(os.open(token_file, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600), "w") as stream:
            stream.write(node["token"])
        env = {"CHAIN_AGENT_DIR": str(h.root / f"agent-{index}"), "CHAIN_PANEL_URL": h.base,
            "CHAIN_NODE_ID": node["id"], "CHAIN_NODE_TOKEN_FILE": str(token_file),
            "CHAIN_XRAY_BIN": str(h.xray), "SSL_CERT_FILE": str(cert)}
        agents.append(h.launch(f"agent-{index}", h.bin_dir / "chain-agent", env))
        nodes.append(node)
        agent_envs.append(env)
    h.wait(lambda: len(h.state()["nodes"]) == 2 and all(n["online"] and
        n["report"]["system"].get("customer_plan_version") == 1 for n in h.state()["nodes"]), "capable Agents")

    def current(line):
        return next(value for value in h.state()["lines"] if value["id"] == line["id"])

    def synced(lines):
        h.wait(lambda: all(current(line)["sync_status"] == "synced" for line in lines), "policy acknowledgement")
        for env in agent_envs:
            remember_internal_ports(h, Path(env["CHAIN_AGENT_DIR"]))

    def create(name, path):
        return h.request("/api/lines", "POST", {"name": name, "kind": "vless",
            "hops": [{"node_id": nodes[i]["id"], "port": h.port()} for i in path], "access": {"protocol": "socks"}})

    def stored():
        return json.loads(database(h.root / "control" / "panel.db", "SELECT body FROM state WHERE id=1")[0][0])

    def consumption():
        return [database(Path(env["CHAIN_AGENT_DIR"]) / "resources.db",
            "SELECT resource,cycle,used FROM consumption ORDER BY resource") for env in agent_envs]

    customer = h.request("/api/customers", "POST", {"name": "private-customer", "contact": "private-contact", "notes": "private-notes"})
    prices = {"monthly": 2000, "quarterly": None, "yearly": None}
    old = create("原独立租赁", [0])
    h.request(f"/api/rentals/{old['id']}", "POST", {"customer_id": customer["id"], "prices": prices})
    h.request("/api/rentals/renew", "POST", {"line_ids": [old["id"]], "term": "monthly", "request_id": "independent-opening"})
    h.publish()
    synced([old])
    old_rental = current(old)["rental"]
    old_export = h.request(f"/api/lines/{old['id']}/export")

    lines = [create("成员 A · 双倍率", [0, 1]), create("成员 B · 单倍率", [1])]
    assert all(current(line)["plan_eligible"] for line in lines)
    plan_input = {"id": "", "name": "真实共享套餐", "customer_id": customer["id"], "monthly_gib": 0.125,
        "members": [{"id": "", "name": f"成员 {index}", "line_ids": [line["id"]],
            "monthly_gib": 0.0625 if index == 0 else None} for index, line in enumerate(lines)], "prices": prices}
    invalid = {**plan_input, "members": [{"id": "", "name": "不可迁入", "line_ids": [old["id"]], "monthly_gib": None}]}
    h.request("/api/customer-plans", "POST", invalid, status=400)
    assert not h.state()["customer_plans"] and current(old)["rental"] == old_rental
    h.request("/api/customer-plans", "POST", plan_input)
    plan = h.state()["customer_plans"][0]
    plan_id, plan_key = plan["id"], "plan:" + plan["id"]

    def plan_view():
        return next(value for value in h.state()["customer_plans"] if value["id"] == plan_id)

    for line in lines:
        h.request("/api/rentals/renew", "POST", {"line_ids": [line["id"]], "term": "monthly", "request_id": "cannot-bypass-" + line["id"]}, status=400)
        h.request("/api/lines/operate", "POST", {"line_ids": [line["id"]], "action": "unlimited"}, status=400)
    finance = h.request("/api/security/members", "POST", {"token": admin_token, "name": "验收财务", "role": "finance"})
    ops = h.request("/api/security/members", "POST", {"token": admin_token, "name": "验收运维", "role": "ops"})

    def pay(request_id):
        before_count = len(h.state()["payments"])
        value = {"line_ids": [plan_key], "term": "monthly", "request_id": request_id, "note": "隔离验收模拟人工到账"}
        order = h.request("/api/rentals/orders", "POST", value)
        assert h.request("/api/rentals/orders", "POST", value)["id"] == order["id"]
        assert h.request(f"/api/rentals/orders/by-request/{request_id}")["id"] == order["id"]
        assert len(h.state()["payments"]) == before_count
        receipt = h.request(f"/api/rentals/orders/{order['id']}/confirm", "POST", {})["payments"]
        assert h.request(f"/api/rentals/orders/{order['id']}/confirm", "POST", {})["payments"] == receipt
        assert len(receipt) == 1 and receipt[0]["line_id"] == plan_key
        assert len(h.state()["payments"]) == before_count + 1
        return receipt[0], order

    with login_as(h, finance["token"]):
        opening, opening_order = pay("plan-opening")
        assert opening_order["created_by"]["id"] == finance["member"]["id"]
        assert all(not line["access"].get("password") and line["share"] is None for line in h.state()["lines"])
        h.request("/api/lines/operate", "POST", {"line_ids": [old["id"]], "action": "disable"}, status=403)
    assert all(current(line)["rental"]["started_at"] > 0 and not current(line)["published"] for line in lines)
    # Same local HTTP/1 TLS fixture as smoke.py; it deliberately cannot advertise H2 preflight.
    h.publish(allow_unverified_reality=True)
    synced([old, *lines])
    assert [current(line)["multiplier"] for line in lines] == [2, 1]
    print("PASS independent rights stay independent; finance opens one shared order exactly once", flush=True)

    payload = b"customer-plan-payload" * 128
    for line in lines:
        echo(line, echo_port, payload)
    expected = [4 * len(payload), 2 * len(payload)]
    for index, rows in enumerate(consumption()):
        used = {resource: value for resource, _, value in rows}
        assert used[plan_key] == expected[index], (index, used, expected)
        member_key = f"member:{plan_id}:{plan['members'][index]['id']}"
        assert used[member_key] == expected[index]
    # A later grant reports prior consumption; a quiet entry does not push this ledger on heartbeat.
    refresh_at = time.monotonic() + 10
    h.wait(lambda: time.monotonic() >= refresh_at, "grant reporting interval", timeout=12)
    for line in lines:
        echo(line, echo_port, b"x")
    h.wait(lambda: plan_view()["usage"]["used"] == sum(expected), "shared usage from both Agents")
    assert [member["usage"]["used"] for member in plan_view()["members"]] == expected
    assert sum(expected) < plan_view()["usage"]["reserved"] <= 128 * 1024 * 1024
    exports = [h.request(f"/api/lines/{line['id']}/export") for line in lines]

    def link(path):
        return h.request(path, "POST", {})["path"].split("#token=", 1)[1]

    def public(token):
        return h.request("/api/public/lookup", "POST", {"token": token}, authenticated=False)

    plan_path = f"/api/customer-plans/{plan_id}/query-link"
    member_path = f"/api/customer-plans/{plan_id}/members/{plan['members'][0]['id']}/query-link"
    plan_token, member_token = link(plan_path), link(member_path)
    whole, member = public(plan_token), public(member_token)
    assert {line["id"] for line in whole["lines"]} == {line["id"] for line in lines}
    assert len(whole["customer_plans"][0]["members"]) == 2
    assert [line["id"] for line in member["lines"]] == [lines[0]["id"]]
    assert [m["id"] for m in member["customer_plans"][0]["members"]] == [plan["members"][0]["id"]]
    serialized = json.dumps(member)
    for forbidden in [customer["id"], customer["name"], customer["contact"], customer["notes"],
            plan["members"][1]["id"], "line_ids", "password", "token_hash"]:
        assert forbidden not in serialized
    config_lookup = h.request("/api/public/lookup", "POST", {"config": exports[0]["share"]}, authenticated=False)
    assert len(config_lookup["customer_plans"][0]["members"]) == 1
    h.request("/api/login", "POST", {"token": member_token}, status=401, authenticated=False)
    old_token, member_token = member_token, link(member_path)
    h.request("/api/public/lookup", "POST", {"token": old_token}, status=404, authenticated=False)
    assert public(member_token)["lines"][0]["id"] == lines[0]["id"]
    with login_as(h, ops["token"]):
        view = h.state()
        assert view["customers"] == view["payments"] == view["customer_plans"] == []
        h.request("/api/rentals/orders", "POST", {"line_ids": [plan_key], "term": "monthly", "request_id": "ops-denied"}, status=403)
    print("PASS weighted real traffic, shared/member ledgers and private query/role isolation", flush=True)

    before_renewal = stored()
    before_consumption = consumption()
    with login_as(h, finance["token"]):
        renewal, _ = pay("plan-renewal")
    synced(lines)
    after_renewal = stored()
    assert after_renewal["rentals"][plan_key]["started_at"] == before_renewal["rentals"][plan_key]["started_at"]
    assert after_renewal["rentals"][plan_key]["expires_at"] > before_renewal["rentals"][plan_key]["expires_at"]
    assert after_renewal["customer_ledgers"] == before_renewal["customer_ledgers"] and consumption() == before_consumption
    assert renewal["previous_expires_at"] == before_renewal["rentals"][plan_key]["expires_at"]
    assert current(old)["rental"] == old_rental

    refund = {"payment_id": opening["id"], "kind": "refund", "amount_fen": 500, "expected_net_fen": 2000,
        "note": "验收模拟线下已退 5 元", "disable_line": False, "request_id": "refund-keep"}
    with login_as(h, finance["token"]):
        first = h.request("/api/rentals/adjustments", "POST", refund)
        assert h.request("/api/rentals/adjustments", "POST", refund) == first
        assert h.request("/api/rentals/adjustments/by-request/refund-keep") == first
    assert all(current(line)["enabled"] for line in lines)
    echo(lines[1], echo_port, b"after-keep")
    lease, ledger = stored()["rentals"][plan_key], stored()["customer_ledgers"]
    final_refund = {**refund, "amount_fen": 1500, "expected_net_fen": 1500, "note": "验收模拟退还其余 15 元并停用全部成员",
        "disable_line": True, "expected_line_ids": [line["id"] for line in lines], "request_id": "refund-disable"}
    with login_as(h, finance["token"]) as finance_client:
        h.request("/api/rentals/adjustments", "POST", {**final_refund, "expected_line_ids": [lines[0]["id"]]}, status=400)
        last = h.request("/api/rentals/adjustments", "POST", final_refund)
        assert h.request("/api/rentals/adjustments", "POST", final_refund) == last
        assert h.request("/api/rentals/adjustments", "POST", refund) == first
        h.request("/api/rentals/adjustments", "POST", {**refund, "request_id": "excess-refund", "expected_net_fen": 0}, status=400)
    synced(lines)
    assert set(last["affected_line_ids"]) == {line["id"] for line in lines}
    assert stored()["rentals"][plan_key] == lease and stored()["customer_ledgers"] == ledger
    assert len(h.state()["payment_adjustments"]) == 2
    for line in lines:
        assert current(line)["enabled"] is False
        with socket.create_connection(("127.0.0.1", line["hops"][0]["port"]), timeout=5) as connection:
            with contextlib.suppress(ConnectionResetError, BrokenPipeError):
                connection.sendall(b"\x05\x01\x02")
                assert connection.recv(2) == b"", "Refund-disabled plan still accepted a connection"
    echo(old, echo_port)
    h.request(f"/api/security/members/{finance['member']['id']}/revoke", "POST", {"token": admin_token})
    with contextlib.ExitStack() as stack:
        original_client = h.client
        stack.callback(setattr, h, "client", original_client)
        h.client = finance_client
        h.request("/api/state", status=401)
    h.request("/api/login", "POST", {"token": finance["token"]}, status=401, authenticated=False)
    print("PASS renewal preserves usage; bounded refunds preserve rights or stop the explicit complete scope", flush=True)

    before_restart, counters = stored(), consumption()
    epochs = [database(Path(env["CHAIN_AGENT_DIR"]) / "resources.db", "SELECT epoch FROM identity") for env in agent_envs]
    for agent in agents:
        h.stop_process(agent)
    h.stop_process(control)
    control = h.launch("control-restarted", h.bin_dir / "chain-panel", control_env)
    h.wait(lambda: h.request("/") or False, "controller restart")
    h.login()
    after_restart = stored()
    for field in ["customers", "customer_plans", "customer_ledgers", "customer_query_tokens", "rentals", "payments",
            "rental_orders", "payment_adjustments", "operator_members", "subscriptions", "lines", "applied_lines"]:
        assert after_restart[field] == before_restart[field], field
    restarted_at = int(time.time())
    for index, env in enumerate(agent_envs):
        h.launch(f"agent-{index}-restarted", h.bin_dir / "chain-agent", env)
    h.wait(lambda: all(n["seen"] > restarted_at and n["report"]["running"] for n in h.state()["nodes"]), "restarted Agent reports")
    synced([old, *lines])
    assert consumption() == counters
    assert [database(Path(env["CHAIN_AGENT_DIR"]) / "resources.db", "SELECT epoch FROM identity") for env in agent_envs] == epochs
    for line, exported in zip(lines, exports):
        lookup = h.request("/api/public/lookup", "POST", {"config": exported["share"]}, authenticated=False)
        assert [value["id"] for value in lookup["lines"]] == [line["id"]], "Original credentials changed on restart"
    assert h.request(f"/api/lines/{old['id']}/export") == old_export
    assert all(line["status"] == "disabled" for line in public(plan_token)["lines"])
    assert public(member_token)["lines"][0]["status"] == "disabled"
    assert all(current(line)["enabled"] is False for line in lines)
    h.request(f"/api/rentals/orders/{opening_order['id']}/confirm", "POST", {})
    assert stored()["payments"] == before_restart["payments"]
    echo(old, echo_port)
    print("ALL CUSTOMER PLAN ACCEPTANCE CHECKS PASSED: restart retained money, budgets, rights and credentials", flush=True)


def main():
    for name in ["CHAIN_TEST_ROOT", "CHAIN_TEST_BIN_DIR", "CHAIN_TEST_XRAY"]:
        if not os.environ.get(name):
            raise RuntimeError(f"{name} must explicitly point to the isolated test environment")
    if hasattr(os, "geteuid") and os.geteuid() == 0:
        raise RuntimeError("Run this acceptance script as an unprivileged user")
    def interrupted(*_):
        raise KeyboardInterrupt()
    signal.signal(signal.SIGTERM, interrupted)
    directory = None
    try:
        with tempfile.TemporaryDirectory(prefix="chain-customer-plans-", dir=os.environ["CHAIN_TEST_ROOT"]) as directory:
            h = Harness(Path(directory))
            try:
                exercise(h)
            except BaseException:
                for log in h.root.glob("*.log"):
                    print(f"LOG {log.name}:\n{log.read_text(errors='replace')[-1800:]}", flush=True)
                raise
            finally:
                try:
                    for agent_dir in h.root.glob("agent-*"):
                        if agent_dir.is_dir():
                            remember_internal_ports(h, agent_dir)
                finally:
                    h.close()
    finally:
        if directory is not None:
            assert not Path(directory).exists(), f"Temporary artifacts remain: {directory}"
            print(f"CLEANUP artifacts removed: {directory}; preserved existing binaries and services", flush=True)


if __name__ == "__main__":
    main()
