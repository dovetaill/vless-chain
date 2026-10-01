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
    elif action == "reload" and os.environ.get("TEST_FAIL_NGINX_RELOAD"):
        marker = root / "reload-failed"
        if service == "nginx" and not marker.exists():
            marker.touch(); sys.exit(1)
    elif action == "stop": status["active"] = False
    elif action == "enable": status["enabled"] = True
    elif action == "disable": status["enabled"] = False
    state_path.write_text(json.dumps(state))
elif name == "xray":
    if args[0] == "version": print("Xray mock")
    elif args[0] == "uuid": print(uuid.uuid4())
    elif args[0] == "x25519":
        import base64, hashlib
        private = args[args.index("-i") + 1] if "-i" in args else base64.urlsafe_b64encode(os.urandom(32)).decode().rstrip("=")
        public = base64.urlsafe_b64encode(hashlib.sha256(private.encode()).digest()).decode().rstrip("=")
        print("PrivateKey: " + private + "\nPassword (PublicKey): " + public)
    elif args[0] == "run":
        config_path = Path(args[args.index("-config") + 1])
        if "-format" not in args and config_path.suffix != ".json":
            print("Failed to get format of " + str(config_path)); sys.exit(23)
        config = json.loads(config_path.read_text())
        if os.environ.get("TEST_FAIL_CONFIG"):
            print("mock configuration rejected"); sys.exit(1)
        assert len({u["id"] for i in config["inbounds"] for u in i["settings"]["clients"]}) == 1
elif name == "ss":
    print("State Recv-Q Send-Q Local Address:Port Peer Address:Port")
    for port in os.environ.get("TEST_LISTEN_PORTS", "").split(","):
        if port: print("LISTEN 0 128 0.0.0.0:" + port + " 0.0.0.0:*")
elif name == "timeout":
    sys.exit(1)
elif name == "curl":
    from urllib.parse import urlsplit
    url = next((arg for arg in args if arg.startswith("http://")), "")
    if url and "-o" in args:
        scope = "LOCAL" if "--resolve" in args else "PUBLIC"
        status = os.environ.get("TEST_HTTP_" + scope + "_STATUS", "200")
        source = Path(os.environ["MANAGER_WEB_ROOT"]) / "domains" / urlsplit(url).hostname
        source /= urlsplit(url).path.lstrip("/")
        content = source.read_bytes() if status == "200" else b"not found"
        if os.environ.get("TEST_HTTP_WRONG_BODY"): content = b"another site"
        Path(args[args.index("-o") + 1]).write_bytes(content)
        print(status, end="")
        sys.exit(int(os.environ.get("TEST_HTTP_CURL_EXIT", "0")))
elif name in ("getent", "dpkg", "sleep", "nginx", "rpm"):
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

    def test_http_preflight_checks_local_and_domain_and_cleans_probe(self):
        self.shell('CERT_MODE=http; write_nginx_challenge_config; verify_http_challenge')
        requests = [event for event in self.events() if event[0] == "curl"]
        self.assertEqual(len(requests), 2)
        self.assertIn("edge.example.com:80:127.0.0.1", requests[0])
        self.assertNotIn("--resolve", requests[1])
        for request in requests:
            self.assertEqual(request[request.index("--noproxy") + 1], "*")
        web = Path(self.env["MANAGER_WEB_ROOT"]) / "domains" / "edge.example.com"
        self.assertEqual(list((web / ".well-known" / "acme-challenge").iterdir()), [])
        self.assertEqual(list(self.root.glob("xray-chain.*")), [])

    def test_http_preflight_failure_stops_before_certbot_and_cleans_probe(self):
        cases = (({"TEST_HTTP_LOCAL_STATUS": "404"}, "本机 HTTP-01 自检失败", 3),
                 ({"TEST_HTTP_PUBLIC_STATUS": "404"}, "域名 HTTP-01 自检失败", 4),
                 ({"TEST_HTTP_WRONG_BODY": "1"}, "本机 HTTP-01 自检失败", 3),
                 ({"TEST_HTTP_CURL_EXIT": "28"}, "本机 HTTP-01 自检失败", 3))
        for values, message, request_count in cases:
            with self.subTest(values=values):
                before = len(self.events())
                output = self.shell('CERT_MODE=http; SECURITY_MODE=tls; '
                                    'certbot() { touch "$TEST_RUNTIME/certbot-called"; }; '
                                    'write_nginx_challenge_config; obtain_or_copy_cert',
                                    values, success=False)
                self.assertIn(message, output)
                self.assertFalse((self.root / "certbot-called").exists())
                requests = [event for event in self.events()[before:] if event[0] == "curl"]
                self.assertEqual(len(requests), request_count)
                web = Path(self.env["MANAGER_WEB_ROOT"]) / "domains" / "edge.example.com"
                self.assertEqual(list((web / ".well-known" / "acme-challenge").iterdir()), [])
                self.assertEqual(list(self.root.glob("xray-chain.*")), [])

    def test_http_preflight_interruption_cleans_probe(self):
        self.shell('CERT_MODE=http; write_nginx_challenge_config; '
                   'trap "exit 143" TERM; curl() { kill -TERM $$; return 1; }; '
                   'verify_http_challenge', success=False)
        web = Path(self.env["MANAGER_WEB_ROOT"]) / "domains" / "edge.example.com"
        self.assertEqual(list((web / ".well-known" / "acme-challenge").iterdir()), [])
        self.assertEqual(list(self.root.glob("xray-chain.*")), [])

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

    def test_main_menu_recovers_current_links_without_saved_export(self):
        import yaml
        app = self.install("forgot", 20801)
        original = json.loads((app / "state.json").read_text())
        (app / "client.txt").unlink()
        stale = dict(original, UUID=UUID, REALITY_PUBLIC_KEY="stale")
        (app / "state.json").write_text(json.dumps(stale))
        output = self.cli(input_text="6\n1\n0\n")
        self.assertIn("查看租户 VLESS 链接", output)
        self.assertIn("vless://" + original["UUID"], output)
        self.assertIn("pbk=" + original["REALITY_PUBLIC_KEY"], output)
        self.assertNotIn("pbk=stale", output)
        self.assertNotIn(original["REALITY_PRIVATE_KEY"], output)
        node_line = next(line for line in output.splitlines() if line.startswith("  - {"))
        node = yaml.safe_load(node_line)[0]
        self.assertEqual(node["reality-opts"]["short-id"], original["REALITY_SHORT_ID"])
        self.assertEqual(node["uuid"], original["UUID"])
        self.assertFalse((app / "client.txt").exists())  # viewing stays read-only

    def test_interactive_edit_returns_to_refreshed_tenant_menu(self):
        app = self.install("rename", 20802)
        original_config = (app / "config.json").read_bytes()
        output = self.cli(input_text='4\n1\n11\n1\n香港 "一号"\n0\n9\n0\n0\n')
        self.assertEqual(json.loads((app / "state.json").read_text())["INSTANCE_NAME"], '香港 "一号"')
        self.assertIn('租户：香港 "一号" (rename)', output)
        self.assertIn(quote('香港 "一号"-reality'), output)
        self.assertEqual((app / "config.json").read_bytes(), original_config)

    def test_credentials_reset_cancellation_and_complete_rotation(self):
        app = self.install("rotate", 20803)
        original = json.loads((app / "state.json").read_text())
        old_config = json.loads((app / "config.json").read_text())
        snapshot = {name: (app / name).read_bytes() for name in ("state.json", "config.json", "client.txt", "reality.env", "install.env")}
        self.cli("reset", "rotate", "all", input_text="\n")
        self.assertEqual(snapshot, {name: (app / name).read_bytes() for name in snapshot})
        self.cli("credentials", "rotate", input_text="4\nrotate\n0\n")
        new = json.loads((app / "state.json").read_text())
        config = json.loads((app / "config.json").read_text())
        for field in ("UUID", "REALITY_PRIVATE_KEY", "REALITY_PUBLIC_KEY", "REALITY_SHORT_ID"):
            self.assertNotEqual(new[field], original[field])
        self.assertEqual(config["inbounds"][0]["settings"]["clients"][0]["id"], new["UUID"])
        self.assertEqual(config["inbounds"][0]["streamSettings"]["realitySettings"]["privateKey"], new["REALITY_PRIVATE_KEY"])
        self.assertEqual(config["outbounds"], old_config["outbounds"])
        self.assertEqual(new["REALITY_PORT"], original["REALITY_PORT"])
        output = self.cli("links", "rotate")
        self.assertIn("vless://" + new["UUID"], output)
        self.assertNotIn(original["UUID"], output)

    def test_reset_restart_failure_restores_every_file(self):
        app = self.install("rollback", 20804)
        names = ("state.json", "config.json", "client.txt", "reality.env", "install.env")
        snapshot = {name: (app / name).read_bytes() for name in names}
        self.cli("reset", "rollback", "all", values={"TEST_FAIL_RESTART": "1"}, input_text="rollback\n", success=False)
        self.assertEqual(snapshot, {name: (app / name).read_bytes() for name in names})
        self.assertEqual(list(app.glob(".config.*")), [])
        self.assertEqual(list(app.glob(".client.*")), [])
        self.assertEqual(list(self.root.glob("xray-chain.*")), [])

    def test_port_edit_preserves_stopped_state_and_blocks_collisions(self):
        app = self.install("ports", 20805)
        other = self.install("other", 20806)
        self.cli("stop", "ports")
        config = json.loads((app / "config.json").read_text())
        self.cli("ports", "ports", input_text="20807\nports\n")
        new = json.loads((app / "config.json").read_text())
        self.assertEqual(new["inbounds"][0]["port"], 20807)
        self.assertEqual(new["outbounds"], config["outbounds"])
        services = json.loads((self.root / "services.json").read_text())
        self.assertFalse(services["xray-chain-ports"]["active"])
        before = (app / "config.json").read_bytes()
        other_before = (other / "config.json").read_bytes()
        self.cli("ports", "ports", input_text="20806\n", success=False)
        self.assertEqual(before, (app / "config.json").read_bytes())
        self.assertEqual(other_before, (other / "config.json").read_bytes())

    def test_duplicate_custom_uuid_is_rejected_without_changes(self):
        app = self.install("first", 20808)
        other = self.install("second", 20809)
        duplicate = json.loads((other / "state.json").read_text())["UUID"]
        snapshot = (app / "config.json").read_bytes()
        self.cli("reset", "first", "custom-uuid", input_text=duplicate + "\nfirst\n", success=False)
        self.assertEqual(snapshot, (app / "config.json").read_bytes())

    def test_delete_confirmed_tenant_preserves_other_and_shared_certificate_site(self):
        cert = self.root / "cert.pem"; key = self.root / "key.pem"
        cert.write_text("fake certificate"); key.write_text("fake key")
        extra = {"SECURITY_MODE": "tls", "TLS_PORT": "20810", "CERT_MODE": "existing", "CERT_FILE": str(cert), "KEY_FILE": str(key)}
        app = self.install("delete", 20810, extra=extra)
        other = self.install("keep", 20811)
        other_config = (other / "config.json").read_bytes()
        shared = Path(self.env["NGINX_CONF_DIR"]) / "xray-chain-http-edge.example.com.conf"
        shared.write_text("shared certificate validation site\n")
        self.cli("delete", "delete", input_text="wrong\n")
        self.assertTrue(app.exists())
        output = self.cli(input_text="4\n1\n13\ndelete\n0\n")
        self.assertIn("已删除租户：delete", output)
        self.assertFalse(app.exists())
        self.assertFalse((Path(self.env["MANAGER_WEB_ROOT"]) / "instances" / "delete").exists())
        self.assertFalse((Path(self.env["SYSTEMD_DIR"]) / "xray-chain-delete.service").exists())
        self.assertEqual((other / "config.json").read_bytes(), other_config)
        self.assertTrue(shared.exists())
        self.assertTrue(cert.exists())
        services = json.loads((self.root / "services.json").read_text())
        self.assertTrue(services["xray-chain-keep"]["active"])
        self.assertFalse(services["xray-chain-delete"]["active"])
        self.assertFalse(services["xray-chain-delete"]["enabled"])

    def test_delete_nginx_reload_failure_restores_service_and_files(self):
        cert = self.root / "cert.pem"; key = self.root / "key.pem"
        cert.write_text("fake certificate"); key.write_text("fake key")
        app = self.install("rollback-delete", 20812, extra={"SECURITY_MODE": "tls", "TLS_PORT": "20812", "CERT_MODE": "existing", "CERT_FILE": str(cert), "KEY_FILE": str(key)})
        unit = Path(self.env["SYSTEMD_DIR"]) / "xray-chain-rollback-delete.service"
        conf = Path(self.env["NGINX_CONF_DIR"]) / "xray-chain-rollback-delete-edge.example.com.conf"
        snapshot = {p: p.read_bytes() for p in (app / "config.json", app / "state.json", unit, conf)}
        self.cli("delete", "rollback-delete", values={"TEST_FAIL_NGINX_RELOAD": "1"}, input_text="rollback-delete\n", success=False)
        self.assertEqual(snapshot, {p: p.read_bytes() for p in snapshot})
        services = json.loads((self.root / "services.json").read_text())
        self.assertTrue(services["xray-chain-rollback-delete"]["active"])
        self.assertTrue(services["xray-chain-rollback-delete"]["enabled"])

    def test_imported_delete_only_unregisters_original_deployment(self):
        app = self.install("legacy", 20813)
        record = app / "state.json"
        state = json.loads(record.read_text())
        original = self.root / "original"
        original.mkdir()
        source_config = original / "config.json"
        source_config.write_bytes((app / "config.json").read_bytes())
        state.update(INSTANCE_IMPORTED="true", APP_DIR=str(original))
        record.write_text(json.dumps(state))
        before = source_config.read_bytes()
        self.cli("delete", "legacy", input_text="legacy\n")
        self.assertFalse(record.exists())
        self.assertEqual(source_config.read_bytes(), before)
        services = json.loads((self.root / "services.json").read_text())
        self.assertTrue(services["xray-chain-legacy"]["active"])

    def test_dual_reset_and_exports_use_same_uuid_and_both_ports(self):
        cert = self.root / "cert.pem"; key = self.root / "key.pem"
        cert.write_text("fake certificate"); key.write_text("fake key")
        app = self.install("dual", 20814, extra={"SECURITY_MODE": "dual", "TLS_PORT": "20815", "CERT_MODE": "existing", "CERT_FILE": str(cert), "KEY_FILE": str(key)})
        self.cli("reset", "dual", "uuid", input_text="dual\n")
        config = json.loads((app / "config.json").read_text())
        ids = {client["id"] for inbound in config["inbounds"] for client in inbound["settings"]["clients"]}
        self.assertEqual(len(ids), 1)
        output = self.cli("links", "dual")
        self.assertEqual(output.count("vless://"), 2)
        self.assertIn(":20814?", output)
        self.assertIn(":20815?", output)


if __name__ == "__main__":
    unittest.main(verbosity=2)
