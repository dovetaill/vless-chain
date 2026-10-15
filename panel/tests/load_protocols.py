#!/usr/bin/env python3
"""Bounded loopback protocol/load acceptance using real Xray and Mihomo cores.

Run with CHAIN_TEST_ROOT and CHAIN_TEST_BIN_DIR. Every child belongs to Harness;
only the JSON report is retained in CHAIN_TEST_ROOT for the invoking task.
exercise(h, node, agent_dir=None, profiles=PROFILES, stages=STAGES) is reusable.
The target must have no saved proxy lines; existing external configurations stay intact.
"""
import argparse
import base64
import concurrent.futures
import contextlib
import copy
import json
import os
from pathlib import Path
import secrets
import signal
import socket
import socketserver
import struct
import sys
import tempfile
import threading
import time
import urllib.request
import uuid

sys.dont_write_bytecode = True
from chain_matrix import PROFILES
from features import receive
from rentals import remember_internal_ports
from smoke import Harness, UdpEcho

STAGES = ("basic", "auth", "load", "rate", "quota")
CHUNK = b"x" * 65536


class TransferServer(socketserver.ThreadingTCPServer):
    daemon_threads = True
    request_queue_size = 256
    accepted = 0

    def verify_request(self, request, client_address):
        self.accepted += 1
        return True


class Transfer(socketserver.BaseRequestHandler):
    def handle(self):
        self.request.settimeout(20)
        with contextlib.suppress(OSError):
            header = receive(self.request, 5)
            if len(header) != 5:
                return
            mode, remaining = header[:1], struct.unpack("!I", header[1:])[0]
            if remaining > 8 * 1024 * 1024:
                return
            total = remaining
            while remaining:
                if mode == b"D":
                    data = CHUNK[:min(remaining, len(CHUNK))]
                else:
                    data = self.request.recv(min(remaining, len(CHUNK)))
                    if not data:
                        return
                if mode in (b"E", b"D"):
                    self.request.sendall(data)
                remaining -= len(data)
            if mode == b"U":
                self.request.sendall(struct.pack("!Q", total))


def connect(port, destination, timeout=12):
    connection = socket.create_connection(("127.0.0.1", port), timeout=timeout)
    try:
        connection.sendall(b"\x05\x01\x00")
        if receive(connection, 2) != b"\x05\x00":
            raise OSError("Local SOCKS negotiation failed")
        connection.sendall(b"\x05\x01\x00\x01" + socket.inet_aton("127.0.0.1") + struct.pack("!H", destination))
        reply = receive(connection, 10)
        if len(reply) != 10 or reply[:2] != b"\x05\x00":
            raise OSError("Proxy rejected TCP connection")
        return connection
    except BaseException:
        connection.close()
        raise


def transfer(port, destination, mode="E", size=65536, seconds=0, timeout=12):
    sent = received = chunks = 0
    started = time.monotonic()
    try:
        with connect(port, destination, timeout) as connection:
            connection.sendall(mode.encode() + struct.pack("!I", size))
            sent += 5
            remaining = size
            deadline = time.monotonic() + seconds if seconds else None
            while remaining and (deadline is None or time.monotonic() < deadline):
                count = min(remaining, len(CHUNK))
                if mode != "D":
                    connection.sendall(CHUNK[:count])
                    sent += count
                if mode in ("E", "D"):
                    value = receive(connection, count)
                    received += len(value)
                    if value != CHUNK[:count]:
                        raise OSError("Payload was truncated or changed")
                remaining -= count
                chunks += 1
            if mode == "U":
                ack = receive(connection, 8)
                received += len(ack)
                if len(ack) != 8 or struct.unpack("!Q", ack)[0] != size:
                    raise OSError("Upload was not acknowledged by the loopback sink")
        return {"ok": True, "sent": sent, "received": received, "chunks": chunks,
            "elapsed": time.monotonic() - started, "stop": "time" if remaining else "bytes"}
    except (OSError, AssertionError) as error:
        return {"ok": False, "sent": sent, "received": received, "chunks": chunks,
            "elapsed": time.monotonic() - started, "error": type(error).__name__ + ": " + str(error)}


def parallel(count, callback):
    ready = threading.Barrier(count)
    def run(index):
        ready.wait(timeout=20)
        return callback(index)
    started = time.monotonic()
    with concurrent.futures.ThreadPoolExecutor(max_workers=count) as pool:
        results = list(pool.map(run, range(count)))
    elapsed = time.monotonic() - started
    return {"concurrency": count, "success": sum(r["ok"] for r in results),
        "failure": sum(not r["ok"] for r in results), "seconds": elapsed,
        "sent_bytes": sum(r.get("sent", 0) for r in results),
        "received_bytes": sum(r.get("received", 0) for r in results),
        "average_mbps": sum(r.get("sent", 0) + r.get("received", 0) for r in results) * 8 / elapsed / 1e6,
        "upload_mbps": sum(r.get("sent", 0) for r in results) * 8 / elapsed / 1e6,
        "download_mbps": sum(r.get("received", 0) for r in results) * 8 / elapsed / 1e6,
        "stop_counts": {key: sum(r.get("stop") == key for r in results) for key in ("time", "bytes")},
        "errors": sorted({r["error"] for r in results if not r["ok"]})}


def exercise(h, node, agent_dir=None, profiles=PROFILES, stages=STAGES):
    state = h.state()
    assert not state["lines"], "Load acceptance requires an empty proxy list; existing lines were preserved"
    system = next(n for n in state["nodes"] if n["id"] == node["id"])["report"]["system"]
    ephemeral = system.get("ephemeral_port_range", [32768, 60999])
    if not (isinstance(ephemeral, list) and len(ephemeral) == 2
            and all(isinstance(value, int) for value in ephemeral) and 0 < ephemeral[0] <= ephemeral[1] <= 65535):
        ephemeral = [32768, 60999]
    # Each direct line needs three internal ports; HY2/TUIC need one additional core bridge.
    needed = 3 * (len(profiles) + 1) + sum(p[1] in ("hysteria2", "tuic") for p in profiles)
    internal = [port for port in range(50000, 65535) if not ephemeral[0] <= port <= ephemeral[1]][:needed]
    assert len(internal) == needed, "The node has too few non-ephemeral internal ports for this matrix"
    occupied = set()
    for table in ("tcp", "tcp6", "udp", "udp6"):
        for entry in Path("/proc/net", table).read_text().splitlines()[1:]:
            fields = entry.split()
            # Closed test connections can retain TIME_WAIT without owning a listener.
            if table.startswith("tcp") and fields[3] == "06":
                continue
            occupied.add(int(fields[1].split(":")[1], 16))
    conflicts = sorted(set(internal) & occupied)
    assert not conflicts, f"Internal test ports are occupied; existing services were preserved: {conflicts}"
    results, created = [], []
    report = Path(os.environ["CHAIN_TEST_ROOT"]) / "load-protocols-results.json"
    metadata = {"node_id": node["id"], "created_lines": [], "internal_candidate_ports": internal, "cleanup": "pending"}
    ports_lock = threading.Lock()
    udp_records, udp_lock = {}, threading.Lock()

    def save_report():
        report.write_text(json.dumps({**metadata, "results": results}, indent=2))

    def remember_line(line):
        created.append(line)
        metadata["created_lines"].append({"id": line["id"], "name": line["name"]})
        save_report()

    def record_port(port):
        with ports_lock:
            h.ports.add(port)
            h.record()

    def free_port():
        for _ in range(200):
            port = 20000 + secrets.randbelow(30000)
            if port in h.ports:
                continue
            with socket.socket() as tcp, socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as udp:
                try:
                    tcp.bind(("0.0.0.0", port))
                    udp.bind(("0.0.0.0", port))
                except OSError:
                    continue
                record_port(port)
                return port
        raise AssertionError("No free TCP/UDP test port below 50000 after 200 attempts")

    def row(profile, stage, **value):
        result = {"profile": profile, "stage": stage, **value}
        results.append(result)
        save_report()
        print("LOAD_CASE " + json.dumps(result), flush=True)

    def counters(line):
        value = next(l for l in h.state()["lines"] if l["id"] == line["id"])
        return value.get("traffic") or {}

    def sampled_after(line, at):
        def fresh():
            value = counters(line)
            return value if value.get("sampled_at", 0) > at else None
        return h.wait(fresh, "fresh data-plane counter sample", timeout=12)

    def accounted(line, before, result):
        after = sampled_after(line, int(time.time()))
        result["meter_upload_bytes"] = after.get("upload", 0) - before.get("upload", 0)
        result["meter_download_bytes"] = after.get("download", 0) - before.get("download", 0)
        result["reported_upload_bps"] = after.get("upload_bps", 0)
        result["reported_download_bps"] = after.get("download_bps", 0)
        return after

    @contextlib.contextmanager
    def udp_association(port, timeout=4):
        with socket.create_connection(("127.0.0.1", port), timeout=5) as control, socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as udp:
            # A fresh source port also avoids reusing the client's cached UDP route after selection changes.
            udp.bind(("127.0.0.1", free_port()))
            control.sendall(b"\x05\x01\x00")
            if receive(control, 2) != b"\x05\x00":
                raise OSError("Local SOCKS negotiation failed")
            control.sendall(b"\x05\x03\x00\x01" + b"\0" * 6)
            reply = receive(control, 10)
            if len(reply) != 10 or reply[:4] != b"\x05\x00\x00\x01":
                raise OSError("Proxy rejected UDP association")
            address, relay = socket.inet_ntoa(reply[4:8]), struct.unpack("!H", reply[8:])[0]
            record_port(relay)
            udp.settimeout(timeout)
            yield udp, ("127.0.0.1" if address == "0.0.0.0" else address, relay)

    def udp_request(port, destination, marker, timeout=4):
        started = time.monotonic()
        try:
            with udp_association(port, timeout) as (udp, relay):
                packet = b"\0\0\0\x01" + socket.inet_aton("127.0.0.1") + struct.pack("!H", destination) + marker
                udp.sendto(packet, relay)
                if udp.recv(65536) != packet:
                    raise OSError("UDP payload was changed")
            return {"ok": True, "sent": len(marker), "received": len(marker), "elapsed": time.monotonic() - started}
        except (OSError, ValueError, struct.error) as error:
            return {"ok": False, "sent": len(marker), "received": 0, "elapsed": time.monotonic() - started,
                "error": type(error).__name__ + ": " + str(error)}

    cert, key = h.root / "load-cert.pem", h.root / "load-key.pem"
    certificate = h.launch("load-certificate", "openssl", {}, ["req", "-x509", "-newkey", "ec", "-pkeyopt",
        "ec_paramgen_curve:prime256v1", "-nodes", "-keyout", str(key), "-out", str(cert),
        "-days", "1", "-subj", "/CN=localhost", "-addext", "subjectAltName=DNS:localhost"])
    assert certificate.wait(timeout=20) == 0, "Task certificate generation failed"
    target_port, echo_port, udp_port, client_port, api_port, isolation_port = [free_port() for _ in range(6)]
    tcp_target = h.server(TransferServer(("127.0.0.1", echo_port), Transfer))
    class ObservedUdpEcho(UdpEcho):
        def handle(self):
            packet = self.request[0]
            with udp_lock:
                record = udp_records.get(packet[:16])
                if record is not None:
                    record["packets"] += 1
                    record["bytes"] += len(packet)
                    record["sequences"].add(packet[16:24])
            super().handle()
    h.server(socketserver.ThreadingUDPServer(("127.0.0.1", udp_port),
        ObservedUdpEcho if "udp-rights" in stages else UdpEcho))

    def udp_token():
        token = secrets.token_bytes(16)
        with udp_lock:
            udp_records[token] = {"packets": 0, "bytes": 0, "sequences": set()}
        return token

    def udp_observed(token):
        with udp_lock:
            value = udp_records[token]
            return {"received_packets": value["packets"], "received_bytes": value["bytes"],
                "unique_packets": len(value["sequences"]), "duplicates": value["packets"] - len(value["sequences"])}

    def udp_stream(association, token, stream, count, timeout=4):
        udp, relay = association
        udp.settimeout(timeout)
        started, confirmed, attempted = time.monotonic(), 0, 0
        error = None
        try:
            for sequence in range(count):
                if time.monotonic() - started > 20:
                    raise TimeoutError("UDP stream exceeded its 20-second bound")
                marker = token + struct.pack("!II", stream, sequence) + b"x" * 1000
                packet = b"\0\0\0\x01" + socket.inet_aton("127.0.0.1") + struct.pack("!H", udp_port) + marker
                attempted += 1
                udp.sendto(packet, relay)
                if udp.recv(65536) != packet:
                    raise OSError("UDP sequence or payload changed")
                confirmed += 1
        except OSError as failure:
            error = type(failure).__name__ + ": " + str(failure)
        return {"ok": error is None, "sent": confirmed * 1024, "received": confirmed * 1024,
            "attempted_packets": attempted, "confirmed_packets": confirmed, "unconfirmed_packets": attempted - confirmed,
            "elapsed": time.monotonic() - started, **({"error": error} if error else {})}

    def udp_batch(port, token, count, associations=None, timeout=4):
        details = []
        def run(index):
            try:
                if associations is None:
                    with udp_association(port, timeout) as association:
                        value = udp_stream(association, token, index, count, timeout)
                else:
                    value = udp_stream(associations[index], token, index, count, timeout)
            except (OSError, ValueError, struct.error) as error:
                value = {"ok": False, "sent": 0, "received": 0,
                    "attempted_packets": 0, "confirmed_packets": 0, "unconfirmed_packets": 0, "error": str(error)}
            details.append(value)
            return value
        result = parallel(16, run)
        result.update({key: sum(value[key] for value in details)
            for key in ("attempted_packets", "confirmed_packets", "unconfirmed_packets")})
        result["payload_mbps"] = result["received_bytes"] * 8 / result["seconds"] / 1e6
        result["target"] = udp_observed(token)
        return result
    nginx = h.root / "load-nginx.conf"
    nginx.write_text(f"pid {h.root}/load-nginx.pid;\nworker_processes 1;\nerror_log {h.root}/load-nginx-error.log;\n"
        "events { worker_connections 1024; }\nhttp { access_log off;\n"
        f"server {{ listen 127.0.0.1:{target_port} ssl; http2 on; server_name localhost;\n"
        f"ssl_certificate {cert}; ssl_certificate_key {key}; ssl_protocols TLSv1.3;\n"
        "location / { return 200 'loopback-reality-target'; } } }\n")
    h.launch("load-reality-target", "nginx", {}, ["-p", str(h.root), "-c", str(nginx), "-g", "daemon off;"])
    tls = {"mode": "manual", "domain": "localhost", "cert": cert.read_text(), "key": key.read_text()}

    def create(name, profile, limits=None):
        _, protocol, secure, network = profile
        line = h.request("/api/lines", "POST", {"name": "load-verify-" + name, "kind": "vless",
            "hops": [{"node_id": node["id"], "port": free_port()}], "limits": limits or {},
            "access": {"protocol": protocol, "host": "127.0.0.1", "tls": tls if secure else {"mode": "off"},
                "reality": {"target": f"127.0.0.1:{target_port}", "server_name": "localhost"},
                "transport": {"network": network, "path": "/load-protocol", "host": "", "mode": "auto"}}})
        remember_line(line)
        return line

    try:
        lines = {profile[0]: create(profile[0], profile) for profile in profiles}
        isolated = create("isolation", PROFILES[8])
        assert {line["id"] for line in h.state()["lines"]} == {line["id"] for line in created}, \
            "Another proxy draft appeared; refusing to publish it"
        h.publish()
        if agent_dir is not None:
            remember_internal_ports(h, Path(agent_dir))
        proxies, invalid = [], {}
        for name, line in [*lines.items(), ("isolation", isolated)]:
            exported = h.request(f"/api/lines/{line['id']}/export")
            proxy = json.loads(exported["node"][2:])
            proxy["name"] = name
            proxy["server"] = "127.0.0.1"
            proxies.append(proxy)
            fields = ["uuid"] if proxy["type"] in ("vless", "vmess") else ["password"]
            if proxy["type"] == "tuic":
                fields = ["uuid", "password"]
            elif proxy["type"] in ("http", "socks5"):
                fields = ["username", "password"]
            invalid[name] = []
            for field in fields:
                bad = copy.deepcopy(proxy)
                bad["name"] = name + "-wrong-" + field
                bad[field] = str(uuid.uuid4()) if field == "uuid" else "incorrect-authentication"
                if proxy["type"] == "ss":
                    bad[field] = base64.b64encode(secrets.token_bytes(16)).decode()
                proxies.append(bad)
                invalid[name].append(bad["name"])
        secret = secrets.token_hex(16)
        core = os.environ.get("CHAIN_TEST_MIHOMO", str(h.xray.with_name("mihomo")))
        for name, port, selected in [("load-client", client_port, proxies), ("load-isolation-client", isolation_port,
            [next(proxy for proxy in proxies if proxy["name"] == "isolation")])]:
            directory = h.root / name
            directory.mkdir()
            config = {"mixed-port": port, "bind-address": "127.0.0.1", "allow-lan": False, "mode": "rule",
                "log-level": "warning", "proxies": selected, "rules": ["MATCH,isolation"]}
            if name == "load-client":
                config.update({"external-controller": f"127.0.0.1:{api_port}", "secret": secret,
                    "proxy-groups": [{"name": "load-selection", "type": "select", "proxies": [p["name"] for p in proxies]}],
                    "rules": ["MATCH,load-selection"]})
            path = directory / "config.json"
            path.write_text(json.dumps(config))
            path.chmod(0o600)
            h.launch(name, core, {"SSL_CERT_FILE": str(cert)}, ["-f", str(path), "-d", str(directory)])

        def select(name):
            request = urllib.request.Request(f"http://127.0.0.1:{api_port}/proxies/load-selection", method="PUT",
                headers={"Authorization": "Bearer " + secret, "Content-Type": "application/json"},
                data=json.dumps({"name": name}).encode())
            with urllib.request.urlopen(request, timeout=5) as response:
                assert response.status == 204
            return True

        h.wait(lambda: select(next(iter(lines))), "Mihomo controller readiness")
        def isolation_ready():
            with socket.create_connection(("127.0.0.1", isolation_port), timeout=1):
                return True
        h.wait(isolation_ready, "Mihomo isolation listener readiness")
        isolation_baseline = transfer(isolation_port, echo_port, size=262144)
        assert isolation_baseline["ok"], "Isolation control proxy did not start"

        def limits(line, value):
            h.request("/api/lines/operate", "POST", {"line_ids": [line["id"]], "action": "limits", "limits": value})
            h.wait(lambda: next(l for l in h.state()["lines"] if l["id"] == line["id"])["sync_status"] == "synced",
                "rate policy acknowledgement")

        if "udp-rights" in stages:
            sampled_after(isolated, int(time.time()))
            udp_isolation_baseline = udp_batch(isolation_port, udp_token(), 16)
            assert udp_isolation_baseline["failure"] == 0 and udp_isolation_baseline["confirmed_packets"] == 256, udp_isolation_baseline
            sampled_after(isolated, int(time.time()))
            metadata["udp_isolation_baseline"] = udp_isolation_baseline

        def udp_isolation(before, value):
            accounted(isolated, before, value)
            value["baseline_seconds"] = udp_isolation_baseline["seconds"]
            value["unthrottled"] = value["payload_mbps"] > 1.2 and value["seconds"] < max(.75, udp_isolation_baseline["seconds"] * 2)
            value["passed"] = value["failure"] == 0 and value["confirmed_packets"] == 256 \
                and value["target"]["unique_packets"] == 256 and value["target"]["duplicates"] == 0 \
                and value["meter_upload_bytes"] == value["meter_download_bytes"] == 262144 and value["unthrottled"]
            return value

        def udp_rights(name, line):
            limits(line, {})
            for direction in ("upload", "download"):
                # Small acknowledged UDP packets need rate headroom below the durable loopback baseline.
                limits(line, {direction + "_mbps": 1})
                before, isolation_before = counters(line), counters(isolated)
                with concurrent.futures.ThreadPoolExecutor(max_workers=1) as runner:
                    limited = runner.submit(udp_batch, client_port, udp_token(), 64)
                    time.sleep(.2)
                    isolation = udp_batch(isolation_port, udp_token(), 16)
                    result = limited.result(timeout=35)
                accounted(line, before, result)
                result["configured_mbps"] = 1
                result["isolation"] = udp_isolation(isolation_before, isolation)
                result["passed"] = result["failure"] == 0 and result["confirmed_packets"] == 1024 \
                    and result["unconfirmed_packets"] == 0 and 0 < result["payload_mbps"] <= 1.2 \
                    and result["seconds"] >= (1048576 * 8 / 1e6) * .8 \
                    and result["target"]["unique_packets"] == 1024 and result["target"]["duplicates"] == 0 \
                    and result["target"]["received_bytes"] == 1048576 \
                    and result["meter_upload_bytes"] == result["meter_download_bytes"] == 1048576 \
                    and result["isolation"]["passed"]
                row(name, "udp-rate-shared-" + direction, **result)
            limits(line, {})
            with contextlib.ExitStack() as stack:
                associations, warmup_token = [], udp_token()
                for index in range(16):
                    association = stack.enter_context(udp_association(client_port))
                    associations.append(association)
                    warmup = udp_stream(association, warmup_token, index, 1)
                    assert warmup["ok"], warmup
                before = sampled_after(line, int(time.time()))
                cap = before.get("upload", 0) + before.get("download", 0) + 262144
                limits(line, {"monthly_gib": cap / 1073741824})
                token = udp_token()
                result = udp_batch(client_port, token, 64, associations, timeout=2)
                exhausted = accounted(line, before, result)
                probe_token = udp_token()
                old_udp = udp_stream(associations[0], probe_token, 0, 1, timeout=2)
                marker = probe_token + struct.pack("!II", 1, 0) + b"x" * 1000
                new_udp = udp_request(client_port, udp_port, marker, timeout=2)
                accepted_before = tcp_target.accepted
                tcp = transfer(client_port, echo_port, size=1024, timeout=3)
                post = {"existing_udp": old_udp, "new_udp": new_udp, "tcp": tcp,
                    "target_tcp_connections": tcp_target.accepted - accepted_before}
                final = accounted(line, exhausted, post)
                post["target_udp"] = udp_observed(probe_token)
                isolation_before = counters(isolated)
                result["isolation"] = udp_isolation(isolation_before, udp_batch(isolation_port, udp_token(), 16))
                result["target"] = udp_observed(token)
                result["after_exhaustion"] = post
                result["limit_bytes"], result["remaining_bytes_before"] = cap, 262144
                result["expected_quota_stops"] = result.pop("failure")
                result["passed"] = result["confirmed_packets"] > 0 \
                    and result["meter_upload_bytes"] + result["meter_download_bytes"] == 262144 \
                    and exhausted.get("upload", 0) + exhausted.get("download", 0) == cap \
                    and exhausted.get("blocked") is True and final.get("blocked") is True \
                    and 0 < result["target"]["received_bytes"] <= result["meter_upload_bytes"] \
                    and result["target"]["duplicates"] == 0 \
                    and not old_udp["ok"] and not new_udp["ok"] and not tcp["ok"] \
                    and post["target_udp"]["received_packets"] == post["target_tcp_connections"] == 0 \
                    and post["meter_upload_bytes"] == post["meter_download_bytes"] == 0 \
                    and result["isolation"]["passed"]
                row(name, "udp-quota-hard-cap", **result)

        for profile in profiles:
            name, protocol, _, _ = profile
            line = lines[name]
            select(name)
            healthy = True
            if "basic" in stages:
                before = counters(line)
                result = parallel(1, lambda _: transfer(client_port, echo_port, size=16384))
                accounted(line, before, result)
                result["passed"] = result["failure"] == 0 and result["meter_upload_bytes"] >= 16389 and result["meter_download_bytes"] >= 16384
                healthy = result["passed"]
                row(name, "basic-tcp", **result)
                if protocol == "http":
                    row(name, "basic-udp", passed=True, capability="unsupported")
                else:
                    before = counters(line)
                    result = parallel(1, lambda _: udp_request(client_port, udp_port, b"protocol-udp"))
                    accounted(line, before, result)
                    result["passed"] = result["failure"] == 0 and result["meter_upload_bytes"] >= 12 and result["meter_download_bytes"] >= 12
                    healthy = healthy and result["passed"]
                    row(name, "basic-udp", **result)
            if "auth" in stages:
                for wrong in invalid[name]:
                    select(wrong)
                    for transport in ("tcp", "udp") if protocol != "http" else ("tcp",):
                        before = counters(line)
                        result = parallel(1, lambda _: transfer(client_port, echo_port, size=64, timeout=5)
                            if transport == "tcp" else udp_request(client_port, udp_port, b"invalid-auth", timeout=2))
                        accounted(line, before, result)
                        result["passed"] = result["success"] == 0 and result["meter_upload_bytes"] == result["meter_download_bytes"] == 0
                        result["expected_rejections"] = result.pop("failure")
                        row(name, "auth-" + wrong.rsplit("-", 1)[1] + "-" + transport, **result)
                select(name)
            if not healthy:
                row(name, "dependent-load-and-rate", passed=False, skipped="Basic traffic or metering failed; no stress retries")
                continue
            if "load" in stages:
                for count in (1, 16, 64):
                    before = counters(line)
                    result = parallel(count, lambda _: transfer(client_port, echo_port,
                        size=4 * 1024 * 1024 if count == 64 else 65536, seconds=6 if count == 64 else 0))
                    accounted(line, before, result)
                    result["passed"] = result["failure"] == 0 and result["meter_upload_bytes"] >= result["sent_bytes"] and result["meter_download_bytes"] >= result["received_bytes"]
                    row(name, "load-tcp", **result)
                if protocol != "http":
                    for count in (1, 16):
                        before = counters(line)
                        result = parallel(count, lambda index: udp_request(client_port, udp_port, index.to_bytes(2, "big") + b"x" * 1022))
                        accounted(line, before, result)
                        result["passed"] = result["failure"] == 0 and result["meter_upload_bytes"] >= result["sent_bytes"] and result["meter_download_bytes"] >= result["received_bytes"]
                        row(name, "load-udp", **result)
            if "rate" in stages:
                for mode, direction in (("U", "upload"), ("D", "download")):
                    limits(line, {direction + "_mbps": 2})
                    before = counters(line)
                    isolation_before = counters(isolated)
                    with concurrent.futures.ThreadPoolExecutor(max_workers=1) as runner:
                        limited = runner.submit(parallel, 16,
                            lambda _: transfer(client_port, echo_port, mode=mode, size=65536, timeout=20))
                        time.sleep(.3)
                        isolation = transfer(isolation_port, echo_port, size=262144)
                        result = limited.result(timeout=35)
                    accounted(line, before, result)
                    isolation_after = sampled_after(isolated, int(time.time()))
                    actual = result["sent_bytes"] - 16 * 5 if mode == "U" else result["received_bytes"]
                    result["payload_mbps"] = actual * 8 / result["seconds"] / 1e6
                    result["configured_mbps"] = 2
                    result["isolation"] = {"ok": isolation["ok"], "seconds": isolation["elapsed"],
                        "baseline_seconds": isolation_baseline["elapsed"],
                        "meter_upload_bytes": isolation_after.get("upload", 0) - isolation_before.get("upload", 0),
                        "meter_download_bytes": isolation_after.get("download", 0) - isolation_before.get("download", 0)}
                    result["isolation"]["unthrottled"] = isolation["elapsed"] < max(.75, isolation_baseline["elapsed"] * 8)
                    result["passed"] = result["failure"] == 0 and 0 < result["payload_mbps"] <= 2.4 and result["seconds"] >= (1048576 * 8 / 2e6) * .8 and isolation["ok"] and result["isolation"]["unthrottled"]
                    row(name, "rate-shared-" + direction, **result)
                limits(line, {})

            if "quota" in stages:
                before = sampled_after(line, int(time.time()))
                cap = before.get("upload", 0) + before.get("download", 0) + 262144
                limits(line, {"monthly_gib": cap / 1073741824})
                result = parallel(16, lambda index: transfer(client_port, echo_port,
                    mode="U" if index % 2 == 0 else "D", size=131072, timeout=8))
                after = accounted(line, before, result)
                isolated_result = transfer(isolation_port, echo_port, size=131072)
                result["passed"] = after.get("upload", 0) + after.get("download", 0) == cap and after.get("blocked") is True and result["success"] < 16 and isolated_result["ok"]
                result["limit_bytes"] = cap
                result["remaining_bytes_before"] = 262144
                result["isolation"] = isolated_result
                result["expected_quota_stops"] = result.pop("failure")
                row(name, "quota-concurrent-upload-download", **result)
            if "udp-rights" in stages:
                if protocol == "http":
                    row(name, "udp-rights", passed=True, capability="unsupported")
                else:
                    udp_rights(name, line)
        failed = [r for r in results if not r.get("passed", False)]
        print("LOAD_SUMMARY " + json.dumps({"checks": len(results), "failed": len(failed), "report": str(report)}), flush=True)
        assert not failed, "Protocol/load checks failed: " + ", ".join(r["profile"] + "/" + r["stage"] for r in failed)
        return results
    finally:
        if agent_dir is not None:
            remember_internal_ports(h, Path(agent_dir))
        if created:
            h.request("/api/lines/operate", "POST", {"line_ids": [line["id"] for line in created], "action": "delete"})
            def removed():
                state = h.state()
                if any(line["id"] in {created_line["id"] for created_line in created} for line in state["lines"]):
                    return False
                current = next(n for n in state["nodes"] if n["id"] == node["id"])
                return not current.get("desired_revision") or (current["report"]["revision"] == current["desired_revision"]
                    and current["report"]["applied"])
            h.wait(removed, "load fixture removal")
        metadata["cleanup"] = "test lines removed and agent acknowledged; caller must close Harness"
        save_report()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--profiles", nargs="+", choices=[p[0] for p in PROFILES])
    parser.add_argument("--stages", nargs="+", choices=(*STAGES, "udp-rights"), default=STAGES)
    args = parser.parse_args()
    profiles = [p for p in PROFILES if not args.profiles or p[0] in args.profiles]
    os.environ.setdefault("CHAIN_TEST_XRAY", "/opt/chain/releases/0.2.6-linux-x86_64-329ef722a8b6/bin/xray")
    def interrupted(*_):
        raise KeyboardInterrupt()
    signal.signal(signal.SIGTERM, interrupted)
    directory = None
    try:
        with tempfile.TemporaryDirectory(prefix="chain-load-", dir=os.environ["CHAIN_TEST_ROOT"]) as directory:
            h = Harness(Path(directory))
            try:
                port = h.port()
                h.base = f"http://127.0.0.1:{port}"
                h.launch("load-panel", h.bin_dir / "chain-panel", {"CHAIN_DATA_DIR": str(h.root / "control"),
                    "CHAIN_LISTEN": f"127.0.0.1:{port}", "CHAIN_XRAY_BIN": str(h.xray),
                    "SSL_CERT_FILE": str(h.root / "load-cert.pem")})
                h.wait(lambda: (h.root / "control/admin-token").exists() and h.request("/") or False, "controller startup")
                h.login()
                node = h.request("/api/nodes", "POST", {"name": "protocol-load", "host": "127.0.0.1", "target": "127.0.0.1:443", "server_name": "localhost"})
                token = h.root / "node.token"
                token.write_text(node["token"])
                token.chmod(0o600)
                agent_dir = h.root / "agent"
                h.launch("load-agent", h.bin_dir / "chain-agent", {"CHAIN_AGENT_DIR": str(agent_dir), "CHAIN_PANEL_URL": h.base,
                    "CHAIN_NODE_ID": node["id"], "CHAIN_NODE_TOKEN_FILE": str(token), "CHAIN_XRAY_BIN": str(h.xray),
                    "SSL_CERT_FILE": str(h.root / "load-cert.pem")})
                h.wait(lambda: any(n["id"] == node["id"] and n["online"] for n in h.state()["nodes"]), "Agent registration")
                exercise(h, node, agent_dir, profiles, args.stages)
            except BaseException:
                for log in h.root.glob("*.log"):
                    print(f"LOG {log.name}:\n{log.read_text(errors='replace')[-2000:]}", flush=True)
                raise
            finally:
                h.close()
    finally:
        if directory is not None:
            assert not Path(directory).exists(), f"Load-test artifacts remain: {directory}"
            print(f"CLEANUP artifacts removed: {directory}; report retained in CHAIN_TEST_ROOT", flush=True)


if __name__ == "__main__":
    main()
