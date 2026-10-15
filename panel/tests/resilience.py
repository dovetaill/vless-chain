#!/usr/bin/env python3
"""Crash and entitlement acceptance using only task-owned real processes.

Run with CHAIN_TEST_ROOT, CHAIN_TEST_BIN_DIR and CHAIN_TEST_XRAY, as rentals.py.
Calendar-boundary fixtures are written only while this test's panel and Agent
are stopped. No system clock, installed service or firewall is changed.
RESULT lines quantify durable-counter loss and require no loss of confirmed payload.
"""
import calendar
import concurrent.futures
import contextlib
import ctypes
import datetime as dt
import json
import os
from pathlib import Path
import signal
import socket
import socketserver
import sqlite3
import struct
import sys
import tempfile
import time

sys.dont_write_bytecode = True
from features import receive
from rentals import RentalEcho, echo, remember_internal_ports, socks
from smoke import Harness, TcpServer, UdpEcho


def process_info(pid):
    try:
        fields = Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()
        return {"pid": pid, "state": fields[0], "ppid": int(fields[1]),
                "pgid": int(fields[2]), "start": fields[19],
                "command": Path(f"/proc/{pid}/cmdline").read_bytes().replace(b"\0", b" ").decode()}
    except FileNotFoundError:
        return None


class CrashHarness(Harness):
    def __init__(self, root):
        self.children = {}
        super().__init__(root)
        # Reap our cores after deliberately killing their Agent. Otherwise a
        # container's PID 1 may leave orphan zombies in the owned process group.
        self.libc = ctypes.CDLL(None, use_errno=True)
        self.previous_subreaper = ctypes.c_int()
        assert self.libc.prctl(37, ctypes.byref(self.previous_subreaper), 0, 0, 0) == 0
        assert self.libc.prctl(36, 1, 0, 0, 0) == 0

    def record(self):
        super().record()
        path = self.root / "ownership.json"
        value = json.loads(path.read_text())
        value["owned_children"] = list(self.children.values())
        path.write_text(json.dumps(value))

    def register_cores(self, agent):
        assert any(p is agent for _, p, _ in self.processes)
        found = []
        for task in Path(f"/proc/{agent.pid}/task").iterdir():
            try:
                children = (task / "children").read_text().split()
            except FileNotFoundError:
                continue
            for child in children:
                info = process_info(int(child))
                if info and info["ppid"] == agent.pid and info["pgid"] == agent.pid:
                    assert str(self.root) in info["command"], info
                    self.children[info["pid"]] = info
                    found.append(info["pid"])
        self.record()
        return sorted(set(found))

    def reap(self):
        for pid in self.children:
            with contextlib.suppress(ChildProcessError, ProcessLookupError):
                os.waitpid(pid, os.WNOHANG)

    def children_gone(self, pids):
        self.reap()
        return all(process_info(pid) is None for pid in pids)

    def kill_process(self, process):
        assert any(p is process for _, p, _ in self.processes)
        assert process.poll() is None, "Refusing to signal an exited/reused process"
        os.kill(process.pid, signal.SIGKILL)
        process.wait(timeout=5)
        assert process.returncode == -signal.SIGKILL

    def kill_core(self, pid):
        saved, current = self.children[pid], process_info(pid)
        assert current and current["start"] == saved["start"] and current["pgid"] == saved["pgid"]
        os.kill(pid, signal.SIGKILL)

    def stop_process(self, process):
        super().stop_process(process)
        owned = [pid for pid, info in self.children.items() if info["pgid"] == process.pid]
        self.wait(lambda: self.children_gone(owned), "owned core exit/reaping", timeout=8)

    def close(self):
        try:
            super().close()
        finally:
            assert self.libc.prctl(36, self.previous_subreaper.value, 0, 0, 0) == 0


def listening(port, udp=False):
    for row in Path("/proc/net/udp" if udp else "/proc/net/tcp").read_text().splitlines()[1:]:
        fields = row.split()
        if int(fields[1].split(":")[1], 16) == port and (udp or fields[3] == "0A"):
            return True
    return False


def total(value):
    return value["total_upload"] + value["total_download"]


def period(value):
    return value["upload"] + value["download"]


def closed(connection):
    connection.settimeout(4)
    try:
        assert connection.recv(1) == b"", "Previously open TCP connection survived its policy"
    except (ConnectionResetError, BrokenPipeError):
        pass


def blocked(line):
    with socket.create_connection(("127.0.0.1", line["hops"][0]["port"]), timeout=3) as connection:
        with contextlib.suppress(ConnectionResetError, BrokenPipeError):
            connection.sendall(b"\x05\x01\x02")
            assert connection.recv(2) == b"", "Blocked proxy accepted a SOCKS handshake"


@contextlib.contextmanager
def udp_session(h, line, destination):
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as udp:
        udp.bind(("127.0.0.1", 0))
        udp.settimeout(4)
        h.ports.add(udp.getsockname()[1])
        with socks(line, udp.getsockname()[1], command=3) as relay:
            h.ports.add(relay[1])
            h.record()
            prefix = b"\x00\x00\x00\x01" + socket.inet_aton("127.0.0.1") + struct.pack("!H", destination)

            def transfer(payload=b"resilience-udp"):
                udp.sendto(prefix + payload, relay)
                assert udp.recv(65536) == prefix + payload

            yield udp, relay, transfer


class Suite:
    def __init__(self, h):
        self.h = h
        self.agent_dir = h.root / "agent"
        self.results = {}
        self.generation = 0

    def line(self, line):
        return next(value for value in self.h.state()["lines"] if value["id"] == line["id"])

    def traffic(self, line, disk=False):
        if disk:
            return json.loads((self.agent_dir / "traffic.json").read_text())[line["id"]]
        return self.line(line)["traffic"]

    def durable(self, line):
        database = self.agent_dir / "traffic.db"
        with sqlite3.connect(f"file:{database}?mode=ro", uri=True) as db:
            row = db.execute("SELECT body FROM traffic WHERE line_id=?", (line["id"],)).fetchone()
        return json.loads(row[0]) if row else None

    def synced(self, line):
        self.h.wait(lambda: self.line(line)["sync_status"] == "synced", "policy acknowledgement")
        remember_internal_ports(self.h, self.agent_dir)
        self.h.register_cores(self.agent)

    def operate(self, line, action, **extra):
        result = self.h.request("/api/lines/operate", "POST", {
            "line_ids": [line["id"]], "action": action, **extra})
        self.synced(line)
        return result

    def start_panel(self):
        self.generation += 1
        self.control = self.h.launch(f"control-{self.generation}", self.h.bin_dir / "chain-panel", self.control_env)
        self.h.wait(lambda: self.h.request("/") or True, "controller start")
        self.h.login()

    def start_agent(self):
        self.generation += 1
        self.agent = self.h.launch(f"agent-{self.generation}", self.h.bin_dir / "chain-agent", self.agent_env)
        self.h.wait(lambda: listening(self.lines[0]["hops"][0]["port"]), "restored Agent listener")
        self.synced(self.lines[0])

    def setup(self):
        h = self.h
        self.echo_port = h.port()
        h.server(TcpServer(("127.0.0.1", self.echo_port), RentalEcho))
        h.server(socketserver.ThreadingUDPServer(("127.0.0.1", self.echo_port), UdpEcho))
        control_port = h.port()
        h.base = f"http://127.0.0.1:{control_port}"
        self.control_env = {"CHAIN_DATA_DIR": str(h.root / "control"),
            "CHAIN_LISTEN": f"127.0.0.1:{control_port}", "CHAIN_XRAY_BIN": str(h.xray),
            "CHAIN_APPLY_TIMEOUT": "12", "CHAIN_XRAY_LOG_LEVEL": "warning"}
        self.start_panel()
        self.node = h.request("/api/nodes", "POST", {"name": "异常恢复验证节点", "host": "127.0.0.1",
            "target": f"127.0.0.1:{self.echo_port}", "server_name": "localhost"})
        token = h.root / "node.token"
        token.write_text(self.node["token"])
        token.chmod(0o600)
        self.agent_env = {"CHAIN_AGENT_DIR": str(self.agent_dir), "CHAIN_PANEL_URL": h.base,
            "CHAIN_NODE_ID": self.node["id"], "CHAIN_NODE_TOKEN_FILE": str(token), "CHAIN_XRAY_BIN": str(h.xray)}
        self.agent = h.launch("agent-initial", h.bin_dir / "chain-agent", self.agent_env)
        h.wait(lambda: h.state()["nodes"][0]["online"], "Agent registration")
        self.lines = [h.request("/api/lines", "POST", {"name": name, "kind": "vless",
            "hops": [{"node_id": self.node["id"], "port": h.port()}], "access": {"protocol": "socks"}})
            for name in ["崩溃计量", "过期断开", "跨周期保活", "周期恢复额度"]]
        self.main, self.expiring, self.cycling, self.exhausted = self.lines
        h.publish()
        customer = h.request("/api/customers", "POST", {"name": "恢复验证客户", "contact": "test-contact"})
        for line in self.lines:
            h.request(f"/api/rentals/{line['id']}", "POST", {"customer_id": customer["id"],
                "prices": {"monthly": 1200, "quarterly": 3300, "yearly": 12000}})
        h.request("/api/rentals/renew", "POST", {"line_ids": [line["id"] for line in self.lines],
            "term": "monthly", "request_id": "resilience-opening"})
        for line in self.lines:
            self.synced(line)

    def agent_crash(self):
        h, line = self.h, self.main
        echo(line, self.echo_port, b"durable-baseline")
        h.wait(lambda: total(self.traffic(line, True)) >= 2 * len(b"durable-baseline"), "baseline persisted")
        path = self.agent_dir / "traffic.json"
        old_mtime = path.stat().st_mtime_ns
        h.wait(lambda: path.stat().st_mtime_ns != old_mtime, "next successful statistics save")
        before = self.durable(line)
        payload = b"crash-accounting-" * 16384
        delivered = 0
        cores = h.register_cores(self.agent)
        assert cores, "No owned core registered before Agent kill"
        with socks(line, self.echo_port) as connection:
            for _ in range(8):
                connection.sendall(payload)
                assert receive(connection, len(payload)) == payload
                delivered += 2 * len(payload)
            killed_at = time.time()
            h.kill_process(self.agent)
            closed(connection)
        h.wait(lambda: h.children_gone(cores), "Agent death terminates and reaps its cores", timeout=8)
        durable = self.durable(line)
        actual = total(before) + delivered
        lost = actual - total(durable)
        assert lost == 0, {"expected": actual, "durable": durable, "confirmed": delivered}
        self.results["agent_sigkill_accounting"] = {
            "client_confirmed_payload_bytes": delivered, "expected_lifetime_bytes": actual,
            "last_durable_lifetime_bytes": total(durable), "lost_bytes": lost,
            "lost_upload_bytes": before["total_upload"] + delivered // 2 - durable["total_upload"],
            "lost_download_bytes": before["total_download"] + delivered // 2 - durable["total_download"],
            "loss_fraction_of_confirmed_payload": lost / delivered,
            "durable_source": "traffic.db WAL synchronous=FULL",
            "compatibility_snapshot_lifetime_bytes": total(self.traffic(line, True)),
            "compatibility_snapshot_age_seconds_at_kill": max(0, killed_at - path.stat().st_mtime),
            "scope": "SIGKILL process crash; does not emulate hardware power loss or lost filesystem cache"}
        self.start_agent()
        h.wait(lambda: total(self.traffic(line)) == total(durable), "restored exact durable ledger")
        assert self.traffic(line)["epoch"] == durable["epoch"]
        echo(line, self.echo_port, b"after-agent-restart")
        h.wait(lambda: total(self.traffic(line)) == total(durable) + 2 * len(b"after-agent-restart"), "post-restart accounting")
        print("RESULT " + json.dumps({"agent_sigkill_accounting": self.results["agent_sigkill_accounting"]}), flush=True)
        print("PASS Agent SIGKILL closes streams/cores and restores all confirmed payload without lost billing", flush=True)

    def core_crash(self):
        h = self.h
        self.operate(self.cycling, "disable")
        prior = self.durable(self.exhausted)
        self.operate(self.exhausted, "limits", limits={"monthly_gib": 1 / 1073741824})
        line = self.line(self.exhausted)
        active = json.loads((self.agent_dir / "active.json").read_text())
        spec = next(value for value in active["config"]["_chain"] if value["id"] == self.exhausted["id"])
        diagnostic = {"prior_durable_usage": prior, "requested_gib": 1 / 1073741824,
            "panel_gib": line["limits"]["monthly_gib"], "agent_gib": spec["limits"]["monthly_gib"],
            "panel_quota_bytes": int(line["limits"]["monthly_gib"] * 1073741824),
            "agent_quota_bytes": int(spec["limits"]["monthly_gib"] * 1073741824),
            "enabled": spec["enabled"], "lease": spec["lease"], "status": line["rental_status"]}
        assert prior is None or period(prior) == 0, diagnostic
        assert diagnostic["panel_quota_bytes"] == diagnostic["agent_quota_bytes"] == 1, diagnostic
        with socks(self.exhausted, self.echo_port) as connection:
            connection.sendall(b"quota-before-core-kill")
            closed(connection)
        h.wait(lambda: self.durable(self.exhausted) and period(self.durable(self.exhausted)) == 1,
            "pre-crash exhausted quota is durable")
        active_before = json.loads((self.agent_dir / "active.json").read_text())
        rights = {line["id"]: {field: self.line(line)[field] for field in ("enabled", "limits", "rental")}
            for line in self.lines}
        cores = h.register_cores(self.agent)
        assert len(cores) == 1, cores
        with socks(self.main, self.echo_port) as connection, udp_session(h, self.main, self.echo_port) as (_, relay, transfer):
            connection.sendall(b"before-core-kill")
            assert receive(connection, 16) == b"before-core-kill"
            transfer()
            h.kill_core(cores[0])
            closed(connection)
            h.wait(lambda: not listening(relay[1], udp=True), "core death closes UDP association")
        h.wait(lambda: self.agent.poll() is not None, "Agent exits on core failure for its supervisor")
        assert self.agent.returncode != 0
        h.wait(lambda: h.children_gone(cores), "failed runtime children are reaped")
        restart_at = time.monotonic() + 5
        h.wait(lambda: time.monotonic() >= restart_at, "simulate supervisor RestartSec=5", timeout=7)
        self.start_agent()
        assert json.loads((self.agent_dir / "active.json").read_text()) == active_before
        for line in self.lines:
            assert {field: self.line(line)[field] for field in ("enabled", "limits", "rental")} == rights[line["id"]]
        blocked(self.cycling)
        blocked(self.exhausted)
        echo(self.main, self.echo_port, b"after-supervisor-restart")
        with udp_session(h, self.main, self.echo_port) as (_, _, transfer):
            transfer(b"after-supervisor-restart")
        self.operate(self.cycling, "enable")
        self.results["core_sigkill"] = {"agent_failed_for_supervisor": True,
            "recovery": "Harness simulates supervisor restart after 5 seconds; systemd tested separately",
            "publish_needed": False, "active_revision_preserved": True, "rights_preserved": True}
        print("PASS core SIGKILL closes its runtime; supervisor restart restores TCP/UDP and rights without publish", flush=True)

    def panel_crash(self):
        h = self.h
        request = {"line_ids": [self.main["id"]], "term": "quarterly", "amounts": {self.main["id"]: 3000},
            "request_id": "persisted-renewal", "note": "SIGKILL后重试同一收款"}
        payment = h.request("/api/rentals/renew", "POST", request)["payments"]
        self.synced(self.main)
        before = h.state()
        lease = self.line(self.main)["rental"]
        with socks(self.main, self.echo_port) as connection, udp_session(h, self.main, self.echo_port) as (_, _, transfer):
            h.kill_process(self.control)
            connection.sendall(b"panel-is-offline")
            assert receive(connection, 16) == b"panel-is-offline"
            transfer(b"panel-is-offline")
            database = h.root / "control" / "panel.db"
            with sqlite3.connect(f"file:{database}?mode=ro", uri=True) as db:
                assert db.execute("PRAGMA integrity_check").fetchone()[0] == "ok"
                document = json.loads(db.execute("SELECT body FROM state WHERE id=1").fetchone()[0])
            assert document["payments"] == before["payments"]
            assert document["customers"] == before["customers"]
            self.start_panel()
            assert self.line(self.main)["rental"] == lease
            assert h.state()["payments"] == before["payments"]
            assert h.request("/api/rentals/renew", "POST", request)["payments"] == payment
            assert self.line(self.main)["rental"] == lease
            h.request("/api/rentals/renew", "POST", {**request, "note": "changed"}, status=400)
        concurrent_request = {**request, "request_id": "concurrent-renewal"}
        with concurrent.futures.ThreadPoolExecutor(max_workers=6) as workers:
            responses = list(workers.map(lambda _: h.request("/api/rentals/renew", "POST", concurrent_request)["payments"], range(6)))
        assert all(value == responses[0] for value in responses)
        records = [p for p in h.state()["payments"] if p["request_id"] == concurrent_request["request_id"]]
        assert len(records) == 1
        self.results["panel_sigkill"] = {"sqlite_integrity": "ok", "durable_orders_preserved": True,
            "same_request_replayed_without_extension": True, "concurrent_retries": 6, "created_payments": 1}
        print("PASS panel SIGKILL preserves live TCP/UDP, SQLite data and renewal idempotency, including 6 concurrent retries", flush=True)

    def install_boundaries(self):
        h = self.h
        self.h.stop_process(self.agent)
        self.h.stop_process(self.control)
        # Preserve the exact day/time across the anchor's years, including Feb 29.
        zone = dt.timezone(dt.timedelta(hours=8))
        self.boundary = int(time.time()) + 20
        self.expiry = self.boundary + 10
        date = dt.datetime.fromtimestamp(self.boundary, zone)
        year = date.year - 1
        while date.day > calendar.monthrange(year, date.month)[1]:
            year -= 1
        anchor = int(date.replace(year=year).timestamp())
        previous_month = date.month - 1 or 12
        previous_year = date.year if date.month > 1 else date.year - 1
        previous = int(date.replace(year=previous_year, month=previous_month,
            day=min(date.day, calendar.monthrange(previous_year, previous_month)[1])).timestamp())
        values = {self.expiring["id"]: (self.boundary - 60, self.expiry),
            self.cycling["id"]: (anchor, 0), self.exhausted["id"]: (anchor, 0)}
        database = h.root / "control" / "panel.db"
        with sqlite3.connect(database) as db:
            doc = json.loads(db.execute("SELECT body FROM state WHERE id=1").fetchone()[0])
            desired = next(n for n in doc["nodes"] if n["id"] == self.node["id"])["desired"]
            desired["revision"] = "resilience-calendar-fixture"
            for line_id, (start, expiry) in values.items():
                doc["rentals"][line_id].update(started_at=start, expires_at=expiry, paid_months=0)
                spec = next(s for s in desired["config"]["_chain"] if s["id"] == line_id)
                spec["lease"] = {"cycle_anchor": start, "expires_at": expiry}
                if line_id == self.exhausted["id"]:
                    limits = {"monthly_gib": 64 / 1073741824, "upload_mbps": None, "download_mbps": None}
                    spec["limits"] = limits
                    for collection in (doc["lines"], doc["applied_lines"]):
                        next(l for l in collection if l["id"] == line_id)["limits"] = limits
            db.execute("UPDATE state SET body=? WHERE id=1", (json.dumps(doc),))
        (self.agent_dir / "active.json").write_text(json.dumps(desired))
        path = self.agent_dir / "traffic.json"
        book = json.loads(path.read_text())
        for line in (self.cycling, self.exhausted):
            value = book[line["id"]]
            value.update(cycle_start=previous, cycle_end=self.boundary, upload=64, download=0,
                total_upload=64, total_download=0, upload_bps=0, download_bps=0, connections=0)
            value["days"] = {dt.datetime.fromtimestamp(previous, zone).strftime("%Y-%m-%d"): [64, 0]}
        path.write_text(json.dumps(book))
        with sqlite3.connect(self.agent_dir / "traffic.db") as ledger:
            for line in (self.cycling, self.exhausted):
                ledger.execute("INSERT INTO traffic(line_id,body) VALUES(?,?) ON CONFLICT(line_id) DO UPDATE SET body=excluded.body",
                    (line["id"], json.dumps(book[line["id"]])))
        self.start_panel()
        self.start_agent()
        assert time.time() < self.boundary - 3, "Boundary fixture was not ready in time"

    def boundaries(self):
        self.install_boundaries()
        h = self.h
        with contextlib.ExitStack() as stack:
            live = stack.enter_context(socks(self.cycling, self.echo_port))
            dying = stack.enter_context(socks(self.expiring, self.echo_port))
            _, cycle_relay, cycle_udp = stack.enter_context(udp_session(h, self.cycling, self.echo_port))
            _, expiry_relay, expiry_udp = stack.enter_context(udp_session(h, self.expiring, self.echo_port))
            live.sendall(b"before-reset")
            assert receive(live, 12) == b"before-reset"
            dying.sendall(b"before-expiry")
            assert receive(dying, 13) == b"before-expiry"
            cycle_udp(b"before-reset")
            expiry_udp(b"before-expiry")
            blocked(self.exhausted)
            self.h.kill_process(self.control)
            h.wait(lambda: self.traffic(self.cycling, True)["cycle_start"] == self.boundary,
                "Agent resets the calendar cycle without a panel", timeout=30)
            reset = self.traffic(self.cycling, True)
            assert period(reset) == 0 and total(reset) == 64 + 4 * len(b"before-reset"), reset
            live.sendall(b"after-reset")
            assert receive(live, 11) == b"after-reset"
            cycle_udp(b"after-reset")
            assert listening(cycle_relay[1], udp=True), "Cycle reset closed a valid UDP association"
            echo(self.exhausted, self.echo_port, b"reset-ok")
            h.wait(lambda: time.time() >= self.expiry, "expiry wall clock", timeout=15)
            closed(dying)
            h.wait(lambda: not listening(expiry_relay[1], udp=True), "expiry closes UDP association")
            blocked(self.expiring)
            live.sendall(b"permanent-alive")
            assert receive(live, 15) == b"permanent-alive"
        recovery_started = int(time.time())
        self.start_panel()
        observed = {}

        def fresh_boundary_report():
            state = h.state()
            node = next(n for n in state["nodes"] if n["id"] == self.node["id"])
            observed.update(
                reported=node["report"]["traffic"][self.exhausted["id"]],
                displayed=next(line for line in state["lines"] if line["id"] == self.exhausted["id"])["traffic"],
                durable=self.traffic(self.exhausted, True))
            return all(value["sampled_at"] >= recovery_started for value in observed.values())

        try:
            h.wait(fresh_boundary_report, "fresh Agent statistics after panel recovery")
        except AssertionError as error:
            raise AssertionError(f"No fresh boundary report: {json.dumps(observed)}") from error
        for source, value in observed.items():
            assert (value["cycle_start"], period(value), total(value)) == (self.boundary, 16, 80), {
                "source": source, "expected": [self.boundary, 16, 80], "actual": observed}
        durable = self.durable(self.exhausted)
        assert (durable["cycle_start"], period(durable), total(durable)) == (self.boundary, 16, 80), durable
        assert self.line(self.expiring)["rental_status"] == "expired"
        expiry_total = total(self.traffic(self.expiring))
        h.request("/api/rentals/renew", "POST", {"line_ids": [self.expiring["id"]],
            "term": "monthly", "request_id": "expired-reopening"})
        self.synced(self.expiring)
        h.wait(lambda: self.traffic(self.expiring)["cycle_start"] == self.line(self.expiring)["rental"]["started_at"],
            "expired reopening starts a new cycle")
        reopened = self.traffic(self.expiring)
        assert period(reopened) == 0 and total(reopened) == expiry_total
        assert self.line(self.expiring)["rental"]["started_at"] >= self.expiry
        echo(self.expiring, self.echo_port, b"reopened")
        self.results["calendar_boundary"] = {"boundary_unix": self.boundary, "expiry_unix": self.expiry,
            "panel_offline": True, "open_tcp_udp_survived_reset": True,
            "exhausted_quota_restored": True, "expired_tcp_udp_closed": True,
            "expired_renewal_reset_period_only": True}
        print("PASS real calendar boundary resets usage without dropping valid TCP/UDP; expiry closes both while panel is offline", flush=True)

    def permanent_and_quota(self):
        h, line = self.h, self.cycling
        assert self.line(line)["rental"]["permanent"]
        with socks(line, self.echo_port) as live, udp_session(h, line, self.echo_port) as (_, relay, transfer):
            live.sendall(b"manual-disable")
            assert receive(live, 14) == b"manual-disable"
            transfer()
            self.operate(line, "disable")
            closed(live)
            h.wait(lambda: not listening(relay[1], udp=True), "manual disable closes UDP")
        payments = len(h.state()["payments"])
        self.operate(line, "unlimited")
        assert not self.line(line)["enabled"] and self.line(line)["rental"]["permanent"]
        assert len(h.state()["payments"]) == payments
        blocked(line)
        self.operate(line, "enable")
        baseline = total(self.traffic(line))
        echo(line, self.echo_port, b"unlimited-enabled")
        h.wait(lambda: total(self.traffic(line)) == baseline + 2 * len(b"unlimited-enabled"), "finished streams accounted")
        used = period(self.traffic(line))
        assert used > 0
        self.operate(line, "limits", limits={"monthly_gib": used / 1073741824})
        blocked(line)
        assert period(self.traffic(line)) == used
        self.operate(line, "limits", limits={"monthly_gib": (used + 4096) / 1073741824})
        assert period(self.traffic(line)) == used, "Raising quota reset already used bytes"
        echo(line, self.echo_port, b"quota-raised")
        h.wait(lambda: period(self.traffic(line)) == used + 2 * len(b"quota-raised"), "restored quota retains accounting")
        self.results["permanent_policy"] = {"manual_disable_preserved": True,
            "no_payment_created": True, "quota_increase_preserved_used_bytes": used}
        print("PASS permanent access retains manual disable and quota; raising quota restores without clearing usage", flush=True)

    def run(self):
        self.setup()
        self.agent_crash()
        self.core_crash()
        self.panel_crash()
        self.boundaries()
        self.permanent_and_quota()
        (Path(os.environ["CHAIN_TEST_ROOT"]) / "results" / "resilience.json").write_text(
            json.dumps(self.results, ensure_ascii=False, indent=2))
        print("RESULT " + json.dumps(self.results, ensure_ascii=False), flush=True)
        print("ALL RESILIENCE ACCEPTANCE CHECKS PASSED", flush=True)


def exercise(harness):
    Suite(harness).run()


def main():
    os.environ.setdefault("CHAIN_TEST_XRAY", "/opt/chain/releases/0.2.6-linux-x86_64-329ef722a8b6/bin/xray")
    def interrupted(*_):
        raise KeyboardInterrupt()
    signal.signal(signal.SIGTERM, interrupted)
    directory = None
    try:
        with tempfile.TemporaryDirectory(prefix="chain-resilience-", dir=os.environ["CHAIN_TEST_ROOT"]) as directory:
            harness = CrashHarness(Path(directory))
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
