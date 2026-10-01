#!/usr/bin/env python3
"""Isolated manager checks. All services, package tools and network calls are mocked."""
import json
import os
from pathlib import Path
import select
import shlex
import signal
import subprocess
import tempfile
import time
import unittest
from urllib.parse import quote

SCRIPT = Path(__file__).resolve().parents[1] / "xray-vless-native-v3.sh"
UUID = "0bb1bf54-d087-4931-8baf-a560e28de527"
URI = "vless://" + UUID + "@landing.example.com:8443?security=tls&type=raw&flow=xtls-rprx-vision"
MOCK = r'''#!/usr/bin/env python3
import json, os, sys, uuid
from pathlib import Path
name = Path(sys.argv[0]).name
args = sys.argv[1:]
root = Path(os.environ["TEST_RUNTIME"])
with (root / "events.jsonl").open("a") as f:
    f.write(json.dumps([name] + args) + "\n")
state_path = root / "services.json"
state = json.loads(state_path.read_text()) if state_path.exists() else {}
if name in ("systemctl", "rc-service", "rc-update"):
    if name == "systemctl":
        action = args[0]
        service = args[-1]
        if action == "daemon-reload": sys.exit(0)
    elif name == "rc-service":
        service, action = args[:2]
    else:
        action = args[0]
        if action == "show":
            for service, status in state.items():
                if status.get("enabled"): print(service + " | default")
            sys.exit(0)
        service = args[1]
        action = {"add": "enable", "del": "disable"}[action]
    status = state.setdefault(service, {"active": False, "enabled": False})
    if action in ("is-active", "status"):
        print("active" if status["active"] else "inactive")
        sys.exit(0 if status["active"] else 3)
    if action == "is-enabled": sys.exit(0 if status["enabled"] else 1)
    if action == "cat":
        path = Path(os.environ["SYSTEMD_DIR"]) / (service + ".service")
        if not path.exists(): sys.exit(1)
        print(path.read_text()); sys.exit(0)
    if action in ("start", "restart"):
        marker = root / "restart-failed"
        if action == "restart" and os.environ.get("TEST_FAIL_RESTART") and not marker.exists():
            marker.touch(); sys.exit(1)
        status["active"] = True
    elif action == "stop": status["active"] = False
    elif action == "enable": status["enabled"] = True
    elif action == "disable": status["enabled"] = False
    state_path.write_text(json.dumps(state))
elif name == "xray":
    if args[0] == "version": print("Xray mock")
    elif args[0] == "uuid": print(uuid.uuid4())
    elif args[0] == "x25519":
        print("PrivateKey: " + "A" * 43 + "\nPassword: " + "B" * 43)
    elif args[0] == "run":
        config = json.loads(Path(args[args.index("-config") + 1]).read_text())
        if os.environ.get("TEST_FAIL_CONFIG"): sys.exit(1)
        assert len({u["id"] for i in config["inbounds"] for u in i["settings"]["clients"]}) == 1
elif name == "ss":
    print("State Recv-Q Send-Q Local Address:Port Peer Address:Port")
    for port in os.environ.get("TEST_LISTEN_PORTS", "").split(","):
        if port: print("LISTEN 0 128 0.0.0.0:" + port + " 0.0.0.0:*")
elif name == "timeout":
    sys.exit(1)
elif name in ("curl", "getent", "dpkg", "sleep", "nginx", "rpm"):
    if name == "rpm": sys.exit(1)
elif name == "journalctl":
    print("mock service log")
'''


class ManagerChecks(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix="xray-manager-test-", dir=os.environ.get("TEST_RUN_ROOT"))
        self.root = Path(self.tmp.name)
        self.bin = self.root / "bin"
        self.bin.mkdir()
        for command in ("systemctl", "rc-service", "rc-update", "xray", "ss", "curl",
                        "getent", "dpkg", "sleep", "timeout", "nginx", "rpm", "journalctl",
                        "apt-get", "dnf", "yum", "apk"):
            path = self.bin / command
            path.write_text(MOCK)
            path.chmod(0o755)
        self.env = dict(os.environ, PATH=str(self.bin) + ":" + os.environ["PATH"],
                        TEST_RUNTIME=str(self.root), MANAGER_DIR=str(self.root / "manager"),
                        MANAGER_WEB_ROOT=str(self.root / "web"), SYSTEMD_DIR=str(self.root / "units"),
                        NGINX_CONF_DIR=str(self.root / "nginx"), RENEW_HOOK_DIR=str(self.root / "hooks"),
                        OPENRC_DIR=str(self.root / "init.d"), TMPDIR=str(self.root),
                        XRAY_BIN=str(self.bin / "xray"), INIT_SYSTEM="systemd",
                        DOMAIN="edge.example.com", SECURITY_MODE="reality",
                        REALITY_TARGET_MODE="remote", REALITY_TARGET="www.example.com:443",
                        REALITY_TARGET_EXPLICIT="1", CERT_MODE="none")
        for key in ("UUID", "INSTANCE_ID", "INSTANCE_NAME", "UPSTREAM_URI"):
            self.env.pop(key, None)
        self.processes = []

    def spawn(self, argv, env=None):
        process = subprocess.Popen(argv, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                   stderr=subprocess.STDOUT, env=env or self.env,
                                   cwd=SCRIPT.parent, start_new_session=True)
        self.processes.append(process)
        manifest = os.environ.get("TEST_PROCESS_MANIFEST")
        if manifest:
            try:
                ticks = Path("/proc/%s/stat" % process.pid).read_text().split()[21]
            except FileNotFoundError:
                ticks = None
            with open(manifest, "a") as output:
                output.write(json.dumps({"pid": process.pid, "pgid": process.pid,
                                         "start_ticks": ticks, "cwd": str(SCRIPT.parent),
                                         "temporary_dir": str(self.root), "ports": [], "sockets": []}) + "\n")
        return process

    def run_command(self, argv, values=None, input_text="", success=True):
        env = dict(self.env, **(values or {}))
        process = self.spawn(argv, env)
        output, _ = process.communicate(input_text.encode(), timeout=15)
        text = output.decode()
        if success:
            self.assertEqual(process.returncode, 0, text)
        else:
            self.assertNotEqual(process.returncode, 0, text)
        return text

    def shell(self, code, values=None, **kwargs):
        body = "source " + shlex.quote(str(SCRIPT)) + "; trap cleanup EXIT; " + code
        return self.run_command(["bash", "-c", body], values, **kwargs)

    def cli(self, *args, **kwargs):
        return self.run_command(["bash", str(SCRIPT)] + list(args), **kwargs)

    def parse(self, text, success=True):
        path = self.root / "node.input"
        path.write_text(text)
        output = self.shell("native_helper parse " + shlex.quote(str(path)), success=success)
        return json.loads(output) if success else output

    def install(self, name, port, mode="relay", uri=URI, extra=None):
        values = {"REALITY_PORT": str(port)}
        if mode == "relay":
            values["UPSTREAM_URI"] = uri
        values.update(extra or {})
        self.cli("install", mode, name, values=values)
        return Path(self.env["MANAGER_DIR"]) / "instances" / name

    def events(self):
        path = self.root / "events.jsonl"
        return [json.loads(line) for line in path.read_text().splitlines()] if path.exists() else []

    def tearDown(self):
        for process in self.processes:
            if process.poll() is None:
                os.killpg(process.pid, signal.SIGTERM)
                try:
                    process.wait(timeout=3)
                except subprocess.TimeoutExpired:
                    os.killpg(process.pid, signal.SIGKILL)
                    process.wait(timeout=3)
            for handle in (process.stdin, process.stdout):
                if handle:
                    handle.close()
            try:
                os.killpg(process.pid, 0)
            except ProcessLookupError:
                pass
            else:
                self.fail("Owned process group remains: %s" % process.pid)
        root = self.root
        self.tmp.cleanup()
        self.assertFalse(root.exists())

    def test_uri_percent_encoding_and_xhttp_extra(self):
        extra = {"headers": {"X-Test": 'a"b'}, "xmux": {"maxConcurrency": "16-32"}}
        node = self.parse("vless://" + UUID + "@[2001:db8::1]:443?security=tls&type=xhttp"
                          "&path=%2Ftenant%20a&mode=packet-up&extra=" + quote(json.dumps(extra))
                          + "#%E8%90%BD%E5%9C%B0")
        self.assertEqual(node["UPSTREAM_ADDRESS"], "2001:db8::1")
        self.assertEqual(node["UPSTREAM_XHTTP_PATH"], "/tenant a")
        self.assertEqual(node["UPSTREAM_NAME"], "落地")
        output = json.loads(self.shell("native_helper outbound", node))
        self.assertNotIn("flow", output["settings"])
        self.assertEqual(output["streamSettings"]["xhttpSettings"]["extra"], extra)

    def test_yaml_and_json_reality_xhttp(self):
        node = self.parse('{name: "reality xhttp-Beta", type: vless, server: landing.example.com, '
                          'port: 443, uuid: "' + UUID + '", tls: true, network: xhttp, '
                          'servername: www.example.com, reality-opts: {public-key: "' + "B" * 43 +
                          '", short-id: abcd}, xhttp-opts: {path: /tenant, mode: auto, '
                          'reuse-settings: {max-concurrency: "16-32"}}}')
        self.assertEqual(node["UPSTREAM_SECURITY"], "reality")
        self.assertEqual(node["UPSTREAM_TRANSPORT"], "xhttp")
        self.assertEqual(json.loads(node["UPSTREAM_XHTTP_EXTRA"])["xmux"]["maxConcurrency"], "16-32")
        self.assertEqual(self.parse(json.dumps({"type": "vless", "server": "example.com",
                                             "port": 443, "uuid": UUID}))["UPSTREAM_UUID"], UUID)

    def test_partial_node_does_not_infer_security_from_name(self):
        node = self.parse('{name: "reality xhttp-Beta", server: landing.example.com}')
        self.assertEqual(node["UPSTREAM_PORT"], "")
        self.assertEqual(node["UPSTREAM_UUID"], "")
        self.assertEqual(node["UPSTREAM_SECURITY"], "")
        self.assertEqual(node["UPSTREAM_TRANSPORT"], "")
        text = self.shell("INTERACTIVE=1; complete_upstream; native_helper validate", node,
                          input_text="443\n" + UUID + "\nxhttp\nreality\nwww.example.com\n" + "B" * 43 + "\nabcd\n/tenant\n")
        self.assertEqual(text, "")

    def test_unsupported_and_unsafe_nodes_fail_without_execution(self):
        self.parse("vless://" + UUID + "@example.com:443?security=tls&type=ws", success=False)
        self.parse("!!python/object/apply:os.system ['touch " + str(self.root / "executed") + "']", success=False)
        self.assertFalse((self.root / "executed").exists())
        self.parse('proxies: [{server: one}, {server: two}]', success=False)

    def test_xhttp_does_not_accept_vision_flow(self):
        node = self.parse(URI.replace("type=raw", "type=xhttp") + "&path=%2F")
        self.shell("native_helper validate", node, success=False)

    def test_landing_and_multiple_tenants_coexist(self):
        landing = self.install("landing", 20001, mode="direct")
        first = self.install("tenantA", 20002)
        second = self.install("tenantB", 20003, uri=URI.replace("landing.example.com", "other.example.com"))
        states = [json.loads((path / "state.json").read_text()) for path in (landing, first, second)]
        self.assertEqual(len({state["UUID"] for state in states}), 3)
        self.assertEqual(len({state["SERVICE_NAME"] for state in states}), 3)
        snapshot = (second / "config.json").read_bytes()
        self.cli("stop", "tenantA")
        self.assertEqual((second / "config.json").read_bytes(), snapshot)
        self.cli("install", "direct", "duplicate", values={"REALITY_PORT": "20002"}, success=False)
        self.assertFalse((first.parent / "duplicate").exists())
        self.assertIn("other.example.com", self.cli("list"))
        self.assertEqual((first / "state.json").stat().st_mode & 0o777, 0o600)
        self.assertEqual(first.stat().st_mode & 0o777, 0o700)

    def test_failed_creation_preserves_other_instance(self):
        old = self.install("old", 20101)
        snapshot = (old / "config.json").read_bytes()
        self.cli("install", "direct", "bad", values={"REALITY_PORT": "20102", "TEST_FAIL_CONFIG": "1"}, success=False)
        self.assertEqual((old / "config.json").read_bytes(), snapshot)
        self.assertFalse((old.parent / "bad").exists())
        self.assertFalse((Path(self.env["SYSTEMD_DIR"]) / "xray-chain-bad.service").exists())
        self.assertFalse((Path(self.env["MANAGER_WEB_ROOT"]) / "instances" / "bad").exists())

    def test_upstream_restart_failure_rolls_back_all_files(self):
        first = self.install("first", 20201)
        second = self.install("second", 20202)
        before = {name: (first / name).read_bytes() for name in ("config.json", "state.json", "client.txt")}
        other = (second / "config.json").read_bytes()
        self.cli("upstream", "first", values={"UPSTREAM_URI": URI.replace("landing.example.com", "new.example.com"),
                                             "TEST_FAIL_RESTART": "1"}, success=False)
        for name, content in before.items():
            self.assertEqual((first / name).read_bytes(), content)
        self.assertEqual((second / "config.json").read_bytes(), other)
        self.assertFalse(list(first.glob(".config.*")))

    def test_upstream_update_preserves_uuid_ports_and_stopped_state(self):
        first = self.install("first", 20301)
        before = json.loads((first / "state.json").read_text())
        self.cli("stop", "first")
        self.cli("upstream", "first", values={"UPSTREAM_URI": URI.replace("landing.example.com", "new.example.com")})
        after = json.loads((first / "state.json").read_text())
        self.assertEqual(after["UUID"], before["UUID"])
        self.assertEqual(after["REALITY_PORT"], before["REALITY_PORT"])
        self.assertEqual(after["UPSTREAM_ADDRESS"], "new.example.com")
        status = json.loads((self.root / "services.json").read_text())["xray-chain-first"]
        self.assertFalse(status["active"])
        self.assertTrue(status["enabled"])
        self.cli("disable", "first")
        self.cli("start", "first")
        status = json.loads((self.root / "services.json").read_text())["xray-chain-first"]
        self.assertTrue(status["active"])
        self.assertFalse(status["enabled"])

    def test_legacy_import_does_not_rewrite_original(self):
        old = self.install("original", 20401)
        source = self.root / "legacy"
        source.mkdir()
        for name in ("config.json", "install.env", "reality.env", "client.txt"):
            (source / name).write_bytes((old / name).read_bytes())
        state = json.loads((old / "state.json").read_text())
        (Path(self.env["SYSTEMD_DIR"]) / "old-xray.service").write_text(
            "ExecStart=xray run -config " + str(source / "config.json"))
        # Remove only the fake registration, making this fixture an unregistered old deployment.
        (old / "state.json").unlink()
        snapshot = {name: p.read_bytes() for p in source.iterdir() for name in [p.name]}
        self.cli("import", "legacy", str(source), "old-xray")
        imported = json.loads((old.parent / "legacy" / "state.json").read_text())
        self.assertEqual(imported["UUID"], state["UUID"])
        self.assertEqual(imported["APP_DIR"], str(source))
        self.assertEqual(imported["SERVICE_NAME"], "old-xray")
        for name, content in snapshot.items():
            self.assertEqual((source / name).read_bytes(), content)

    def test_package_adapters_and_detection(self):
        for manager, expected in (("apt", ["apt-get", "install", "-y"]),
                                  ("dnf", ["dnf", "install", "-y"]),
                                  ("yum", ["yum", "install", "-y"]),
                                  ("apk", ["apk", "add", "--no-cache"])):
            self.shell("PACKAGE_MANAGER=" + manager + "; package_install example-package")
            self.assertIn(expected + ["example-package"], self.events())
        for binary, manager in (("apt-get", "apt"), ("dnf", "dnf"), ("yum", "yum"), ("apk", "apk")):
            code = 'need_cmd() { [[ "$1" == ' + shlex.quote(binary) + ' || "$1" == rc-service || "$1" == rc-update ]]; }; '
            code += "INIT_SYSTEM=openrc; detect_platform; printf '%s' \"$PACKAGE_MANAGER\""
            self.assertEqual(self.shell(code), manager)

    def test_manual_xhttp_defaults_and_internal_port_collisions(self):
        data = {"UPSTREAM_ADDRESS": "landing.example.com", "UPSTREAM_PORT": "443",
                "UPSTREAM_UUID": UUID, "UPSTREAM_TRANSPORT": "xhttp", "UPSTREAM_SECURITY": "tls",
                "UPSTREAM_XHTTP_PATH": "/tenant"}
        outbound = json.loads(self.shell("native_helper outbound", data))
        self.assertEqual(outbound["streamSettings"]["xhttpSettings"]["extra"], {})
        ports = self.shell("SECURITY_MODE=tls; TLS_PORT=18080; INTERNAL_HTTP_PORT=18080; "
                           "INTERNAL_HTTPS_PORT=18080; choose_internal_ports; "
                           "printf '%s %s' \"$INTERNAL_HTTP_PORT\" \"$INTERNAL_HTTPS_PORT\"")
        self.assertEqual(ports, "18081 18082")

    def test_working_minimal_tools_are_not_reinstalled(self):
        self.shell('detect_platform() { PACKAGE_MANAGER=dnf; }; '
                   'repair_dpkg_state() { :; }; '
                   'python3() { if [[ "$1" == -c ]]; then return 1; fi; command python3 "$@"; }; '
                   'install_base_tools')
        self.assertIn(["dnf", "install", "-y", "ca-certificates", "python3-pyyaml"], self.events())
        installs = [event for event in self.events() if event[0] == "dnf"]
        self.assertTrue(all("curl" not in event and "coreutils" not in event for event in installs))

    def test_openrc_services_and_instance_logs(self):
        first = self.install("alpine", 20501, extra={"INIT_SYSTEM": "openrc"})
        service = Path(self.env["OPENRC_DIR"]) / "xray-chain-alpine"
        self.assertIn('supervisor="supervise-daemon"', service.read_text())
        self.assertIn(str(first / "config.json"), service.read_text())
        (first / "logs" / "service.log").write_text("selected instance log\n")
        self.assertIn("selected instance log", self.cli("logs", "alpine", values={"INIT_SYSTEM": "openrc"}))
        self.cli("disable", "alpine", values={"INIT_SYSTEM": "openrc"})
        self.assertIn(["rc-update", "del", "xray-chain-alpine", "default"], self.events())

    def test_same_domain_nginx_and_renew_hooks_are_separate(self):
        cert = self.root / "cert.pem"
        key = self.root / "key.pem"
        cert.write_text("fake certificate"); key.write_text("fake key")
        extra = {"SECURITY_MODE": "tls", "TLS_PORT": "20601", "CERT_MODE": "existing",
                 "CERT_FILE": str(cert), "KEY_FILE": str(key)}
        first = self.install("tlsA", 20601, extra=extra)
        extra["TLS_PORT"] = "20602"
        second = self.install("tlsB", 20602, extra=extra)
        configs = list(Path(self.env["NGINX_CONF_DIR"]).glob("*.conf"))
        self.assertEqual(len(configs), 2)
        self.assertNotEqual(json.loads((first / "state.json").read_text())["INTERNAL_HTTP_PORT"],
                            json.loads((second / "state.json").read_text())["INTERNAL_HTTP_PORT"])
        for name in ("tlsA", "tlsB"):
            self.shell("load_instance " + name + "; CERT_MODE=http; setup_cert_renew_hook")
        hooks = list(Path(self.env["RENEW_HOOK_DIR"]).glob("*.sh"))
        self.assertEqual(len(hooks), 2)
        self.assertIn('"${RENEWED_LINEAGE:-}"', hooks[0].read_text())
        before = len(self.events())
        self.run_command(["bash", str(hooks[0])], values={"RENEWED_LINEAGE": "/unrelated/certificate"})
        self.assertEqual(len(self.events()), before)

    def test_failed_certificate_request_preserves_shared_credentials(self):
        credentials = Path(self.env["MANAGER_DIR"]) / "credentials" / "edge.example.com.ini"
        credentials.parent.mkdir(parents=True)
        credentials.write_text("dns_cloudflare_api_token = existing-token\n")
        snapshot = credentials.read_bytes()
        self.shell('INSTALL_PENDING=1; CERT_MODE=cloudflare; SECURITY_MODE=tls; '
                   'CF_API_TOKEN=new-token; certbot() { return 1; }; obtain_or_copy_cert', success=False)
        self.assertEqual(credentials.read_bytes(), snapshot)

    def test_menu_and_follow_log_ctrl_c_return(self):
        first = self.install("alpine", 20701, extra={"INIT_SYSTEM": "openrc"})
        (first / "logs" / "service.log").write_text("live-log-marker\n")
        env = dict(self.env, INIT_SYSTEM="openrc")
        process = self.spawn(["bash", str(SCRIPT), "manage", "alpine"], env)
        process.stdin.write(b"3\n"); process.stdin.flush()
        output = b""
        deadline = time.monotonic() + 8
        while b"live-log-marker" not in output and time.monotonic() < deadline:
            ready, _, _ = select.select([process.stdout], [], [], 0.2)
            if ready:
                output += os.read(process.stdout.fileno(), 4096)
        self.assertIn(b"live-log-marker", output)
        os.killpg(process.pid, signal.SIGINT)
        time.sleep(0.3)
        self.assertIsNone(process.poll())
        process.stdin.write(b"0\n"); process.stdin.flush()
        remaining, _ = process.communicate(timeout=5)
        self.assertEqual(process.returncode, 0, (output + remaining).decode())
        self.assertIn("VLESS 实例管理", self.cli(input_text="0\n"))


if __name__ == "__main__":
    unittest.main(verbosity=2)
