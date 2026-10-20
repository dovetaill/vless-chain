#!/usr/bin/env python3
"""Focused sharing acceptance; non-root, task-owned controller/Agent/Xray only."""
import contextlib
import json
import os
from pathlib import Path
import socket
import socketserver
import struct
import sys
import tempfile
import time

sys.dont_write_bytecode = True
from smoke import Harness, TcpServer, TcpEcho
from rentals import echo, socks, remember_internal_ports
from features import receive


def exercise(h):
    target = h.port()
    h.server(TcpServer(("127.0.0.1", target), TcpEcho))
    port = h.port()
    h.base = f"http://127.0.0.1:{port}"
    h.launch("control", h.bin_dir / "chain-panel", {
        "CHAIN_DATA_DIR": str(h.root / "control"), "CHAIN_LISTEN": f"127.0.0.1:{port}",
        "CHAIN_XRAY_BIN": str(h.xray), "CHAIN_APPLY_TIMEOUT": "9"})
    h.wait(lambda: (h.root / "control/admin-token").exists() and h.request("/"), "controller")
    h.login()
    node = h.request("/api/nodes", "POST", {"name": "sharing", "host": "127.0.0.1"})
    token = h.root / "token"
    token.write_text(node["token"])
    token.chmod(0o600)
    agent_dir = h.root / "agent"
    h.launch("agent", h.bin_dir / "chain-agent", {"CHAIN_AGENT_DIR": str(agent_dir),
        "CHAIN_PANEL_URL": h.base, "CHAIN_NODE_ID": node["id"], "CHAIN_NODE_TOKEN_FILE": str(token),
        "CHAIN_XRAY_BIN": str(h.xray), "CHAIN_INGRESS_STATUS": str(h.root / "no-ingress")})
    h.wait(lambda: any(n["id"] == node["id"] and n["online"] for n in h.state()["nodes"]), "agent")
    line = h.request("/api/lines", "POST", {"name": "sharing", "kind": "vless",
        "hops": [{"node_id": node["id"], "port": h.port()}], "access": {"protocol": "socks"}})
    h.publish()

    def synced():
        h.wait(lambda: next(l for l in h.state()["lines"] if l["id"] == line["id"])["sync_status"] == "synced", "policy")
        remember_internal_ports(h, agent_dir)

    def policy(**settings):
        h.request("/api/access/" + line["id"], "POST", settings)
        synced()

    def denied():
        try:
            echo(line, target)
        except (OSError, AssertionError):
            return
        raise AssertionError("Denied destination was forwarded")

    synced()
    echo(line, target)
    policy(denied_ports=[target])
    denied()
    policy(restricted=True, domains=["example.com"])
    denied()  # A literal IP cannot impersonate a permitted domain.
    policy(max_connections=1)
    with socks(line, target) as first:
        first.sendall(b"held")
        assert receive(first, 4) == b"held"
        denied()
    policy()
    echo(line, target)
    print("PASS real forwarding, forbidden ports, strict IP rejection, connection cap and recovery", flush=True)

    links = h.request("/api/externals/batch", "POST", {"urls": [
        "socks5://u:p@same.example.com:1001#one", "socks5://u:p@same.example.com:1002#two"]})
    candidates = h.request("/api/resources/candidates", "POST", {})
    assert len(candidates) == 1 and len(candidates[0]["ids"]) == 2
    h.request("/api/resources/groups", "POST", {"ids": [links[0]["id"]], "name": "shared", "automatic": True})
    candidates = h.request("/api/resources/candidates", "POST", {})
    assert candidates[0]["ids"] == [links[1]["id"]] and len(candidates[0]["matching"]) == 1
    group = candidates[0]["matching"][0]["id"]
    h.request("/api/resources/groups", "POST", {"ids": [links[0]["id"]], "group": group, "automatic": True}, status=400)
    h.request("/api/resources/groups", "POST", {"ids": [links[1]["id"]], "group": group, "automatic": True})
    assert h.request("/api/resources/candidates", "POST", {}) == []
    h.request("/api/resources/groups", "POST", {"ids": [links[0]["id"]], "automatic": False})
    assert h.request("/api/resources/candidates", "POST", {})[0]["ids"] == [links[0]["id"]]
    print("PASS batch import, candidate grouping, preservation and manual removal", flush=True)

    resource = h.request("/api/resources", "POST", {"name": "tiny", "members": ["node:" + node["id"]],
        "monthly_gib": 128 / 1073741824, "mbps": 0.008, "reset_day": 21, "reset_hour": 0, "timezone": "America/Los_Angeles"})
    synced()
    echo(line, target, b"a" * 32)
    echo(line, target, b"b" * 32)
    denied()
    resource["monthly_gib"] = 4096 / 1073741824
    h.request("/api/resources", "POST", resource)
    start=time.monotonic()
    echo(line, target, b"c" * 512)
    assert time.monotonic()-start >= 0.8, "Shared server bandwidth did not count both directions"
    print("PASS resource hard quota, shared bandwidth and increased quota recovery", flush=True)


if __name__ == "__main__":
    assert os.geteuid() != 0, "Run as an unprivileged identity"
    with tempfile.TemporaryDirectory(prefix="sharing-", dir=os.environ["CHAIN_TEST_ROOT"]) as directory:
        h = Harness(Path(directory))
        try:
            exercise(h)
        finally:
            h.close()
