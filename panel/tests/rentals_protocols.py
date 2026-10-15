"""Only the TLS/QUIC authentication and accounting cases changed by rental metering."""
import json
import os
import socket
import ssl
import struct
import uuid

from features import receive
from mihomo import udp_echo


def tcp_echo(port, destination, payload):
    with socket.create_connection(("127.0.0.1", port), timeout=8) as connection:
        connection.sendall(b"\x05\x01\x00")
        assert receive(connection, 2) == b"\x05\x00"
        connection.sendall(b"\x05\x01\x00\x01" + socket.inet_aton("127.0.0.1") + struct.pack("!H", destination))
        assert receive(connection, 10)[:2] == b"\x05\x00"
        connection.sendall(payload)
        assert receive(connection, len(payload)) == payload


def exercise(h, server, echo_port, agent_dir):
    # Delayed import keeps rentals.py as the only test runner and cleanup owner.
    from rentals import remember_internal_ports

    cert, key = h.root / "cert.pem", h.root / "key.pem"
    certificate = h.launch("test-certificate", "openssl", {}, ["req", "-x509", "-newkey", "ec", "-pkeyopt",
        "ec_paramgen_curve:prime256v1", "-nodes", "-keyout", str(key), "-out", str(cert),
        "-days", "1", "-subj", "/CN=localhost", "-addext", "subjectAltName=DNS:localhost"])
    assert certificate.wait(timeout=20) == 0, "Could not create the task-owned TLS certificate"
    tls = {"mode": "manual", "domain": "localhost", "cert": cert.read_text(), "key": key.read_text()}
    lines = [h.request("/api/lines", "POST", {"name": protocol + "计量验证", "kind": "vless",
        "hops": [{"node_id": server["id"], "port": h.port()}],
        "access": {"protocol": protocol, "host": "127.0.0.1", "tls": tls}})
        for protocol in ["vless", "hysteria2", "tuic"]]
    h.publish()
    remember_internal_ports(h, agent_dir)

    def traffic(line):
        return next(item for item in h.state()["lines"] if item["id"] == line["id"])["traffic"] or {}

    def listening(port):
        with socket.create_connection(("127.0.0.1", port), timeout=1):
            return True

    vless = lines[0]
    h.wait(lambda: traffic(vless).get("sampled_at", 0), "initial VLESS counter sample")
    baseline = traffic(vless)
    invalid = uuid.uuid4()
    assert str(invalid) != vless["hops"][0]["uuid"]
    context = ssl.create_default_context(cafile=str(cert))
    with socket.create_connection(("127.0.0.1", vless["hops"][0]["port"]), timeout=5) as connection:
        with context.wrap_socket(connection, server_hostname="localhost") as secure:
            secure.sendall(b"\x00" + invalid.bytes + b"\x00\x01" + struct.pack("!H", echo_port)
                + b"\x01" + socket.inet_aton("127.0.0.1") + b"must-not-charge")
            try:
                assert secure.recv(64) == b"", "Wrong VLESS UUID reached an authenticated stream"
            except (ConnectionResetError, ssl.SSLEOFError):
                pass
    h.wait(lambda: traffic(vless).get("sampled_at", 0) > baseline["sampled_at"], "counter sample after wrong VLESS UUID")
    assert (traffic(vless)["upload"], traffic(vless)["download"]) == (baseline["upload"], baseline["download"])
    print("PASS real TLS handshake with wrong VLESS UUID does not consume allowance", flush=True)

    core = os.environ.get("CHAIN_TEST_MIHOMO", str(h.xray.with_name("mihomo")))
    for line in lines:
        exported = h.request(f"/api/lines/{line['id']}/export")
        proxy = json.loads(exported["node"][2:])
        directory = h.root / ("meter-client-" + line["access"]["protocol"])
        directory.mkdir()
        port = h.port()
        config = directory / "config.yaml"
        config.write_text(json.dumps({"mixed-port": port, "allow-lan": False, "bind-address": "127.0.0.1",
            "mode": "rule", "log-level": "warning", "proxies": [proxy], "rules": ["MATCH," + proxy["name"]]}))
        client = h.launch(directory.name, core, {"SSL_CERT_FILE": str(cert)}, ["-f", str(config), "-d", str(directory)])
        try:
            h.wait(lambda: listening(port), "Mihomo client listener")
            before = traffic(line)
            payload = (line["access"]["protocol"] + "-metered-tcp").encode() * 128
            tcp_echo(port, echo_port, payload)
            h.wait(lambda: traffic(line).get("upload", 0) >= before.get("upload", 0) + len(payload)
                and traffic(line).get("download", 0) >= before.get("download", 0) + len(payload),
                line["access"]["protocol"] + " authenticated TCP accounting")
            before = traffic(line)
            marker = (line["access"]["protocol"] + "-metered-udp").encode()
            udp_echo(port, echo_port, marker)
            h.wait(lambda: traffic(line).get("upload", 0) >= before.get("upload", 0) + len(marker)
                and traffic(line).get("download", 0) >= before.get("download", 0) + len(marker),
                line["access"]["protocol"] + " authenticated UDP accounting")
            print("PASS " + line["access"]["protocol"] + " real authenticated TCP + UDP accounting", flush=True)
        finally:
            h.stop_process(client)
    h.request("/api/lines/operate", "POST", {"line_ids": [line["id"] for line in lines], "action": "delete"})
    h.wait(lambda: any(n["id"] == server["id"] and n["report"]["applied"]
        and n["report"]["revision"] == n["desired_revision"] for n in h.state()["nodes"]), "temporary protocol removal")
