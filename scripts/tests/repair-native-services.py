"""Exercise transformations and main with private files and fake services/downloads."""
from pathlib import Path
import stat
import tempfile
from types import SimpleNamespace
from unittest.mock import patch
import contextlib
import io
import json
import os
import shutil
import socket
import subprocess
import hashlib

script = Path(__file__).resolve().parents[1] / "repair-native-services.sh"
source = script.read_text().split("<<'PY'\n", 1)[1].rsplit("\nPY", 1)[0]
namespace = {"__name__": "selfcheck"}
exec(compile(source, str(script), "exec"), namespace)

for role in ("ingress", "maintenance"):
    component = "agent" if role == "ingress" else "panel"
    marker = "# chain-ingress managed v1" if role == "ingress" else "# chain-panel-installer-v1"
    original = (marker + "\n[Service]\nExecStart=/usr/bin/python3 -B /opt/chain/current-"
                + component + "/scripts/chain_" + role + ".py"
                + (" apply" if role == "ingress" else "") + "\nPrivateTmp=true\n")
    changed = namespace["unit_text"](original, role)
    assert "scripts/chain_" not in changed and "PrivateTmp=true" in changed
    assert namespace["unit_text"](changed, role) == changed
    if role == "ingress":
        target = namespace["patch_binary"]()
        patched = namespace["unit_text"](changed, role, target)
        assert target in patched and namespace["unit_text"](patched, role, target) == patched
    try:
        namespace["unit_text"](original.replace("/usr/bin/python3", "/custom/python"), role)
    except RuntimeError:
        pass
    else:
        raise AssertionError("accepted custom command")

for role in ("agent", "panel"):
    original = ('# chain-panel-installer-v1\n[Service]\nEnvironmentFile="/etc/chain-'
                + role + "/" + role + '.env"\nProtectSystem=strict\n')
    changed = namespace["environment_text"](original, role)
    assert 'EnvironmentFile="' not in changed and "ProtectSystem=strict" in changed
    assert namespace["environment_text"](changed, role) == changed
    try:
        namespace["environment_text"](original.replace("/etc/chain-", "/custom/chain-"), role)
    except RuntimeError:
        pass
    else:
        raise AssertionError("accepted custom environment")

with tempfile.TemporaryDirectory(prefix="chain-repair-selfcheck-") as directory:
    prefix = Path(directory)
    prefix.joinpath("real").mkdir()
    prefix.joinpath("modules").symlink_to("real")
    module = prefix / "modules/ngx_stream_module.so"
    module.write_bytes(b"fixture")
    original = "# preserve comment\nload_module modules/ngx_stream_module.so;\n"
    with patch.object(Path, "stat", return_value=SimpleNamespace(st_uid=0, st_mode=stat.S_IFREG | 0o644)):
        changed = namespace["module_text"](original, prefix)
    assert changed == '# preserve comment\nload_module "' + str(module.resolve()) + '";\n'
    assert namespace["module_text"](changed, prefix) == changed
assert not Path(directory).exists()

for role in ("ingress", "maintenance"):
    component = "agent" if role == "ingress" else "panel"
    args = ["-B", "/opt/chain/current-" + component + "/scripts/chain_" + role + ".py"]
    if role == "ingress":
        args.append("watch")
    shlex = namespace["shlex"]
    original = ("#!/sbin/openrc-run\n# chain-panel-installer-v1\nsupervisor=supervise-daemon\n"
                "command=/usr/bin/python3\ncommand_args=" + shlex.quote(shlex.join(args))
                + "\nexport PYTHONDONTWRITEBYTECODE=1\nstopgroup=yes\n")
    changed = namespace["openrc_text"](original, role)
    assert namespace["openrc_text"](changed, role) == changed
    assert "export PYTHONDONTWRITEBYTECODE=1\nstopgroup=yes\n" in changed
    assert ("internal-ingress-watch" if role == "ingress" else "maintenance") in changed
    if role == "ingress":
        target = namespace["patch_binary"]()
        patched = namespace["openrc_text"](changed, role, target)
        assert target in patched and namespace["openrc_text"](patched, role, target) == patched


def fixture(kind, roles, missing_ingress=False, enabled_ingress=False, bad_download=False,
            delayed_download=False, fresh_failure=False):
    """Exercise main with private files and fake service commands, as an unprivileged UID."""
    # Keep the real Unix socket below its path limit inside CI's nested TMPDIR.
    with tempfile.TemporaryDirectory(prefix="cr-") as directory:
        root = Path(directory)
        def path(value):
            value = Path(value)
            return root / str(value).lstrip("/") if value.is_absolute() and not value.is_relative_to(root) else value
        def write(name, data, mode=0o644):
            target = path(name)
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes(data.encode() if isinstance(data, str) else data)
            target.chmod(mode)
            return target
        path("/run/systemd/system" if kind == "systemd" else "/run/openrc").mkdir(parents=True)
        write("/opt/chain/.chain-install.json", json.dumps({"owner": "chain-panel-installer-v1", "roles": {role: {"version": "0.2.66"} for role in roles}}))
        write("/opt/chain/.install.lock", "", 0o600)
        write("/sbin/supervise-daemon", b"\x7fELF", 0o700)
        write("/usr/bin/python3", b"\x7fELF", 0o700)
        pids, calls, sockets, downloads = {}, [], [], []
        clock = [0]
        pending_request = [False]
        fresh_error = [fresh_failure]
        request_counter = [0]
        next_pid = [100]
        artifact = bytearray(64)
        artifact[:6], artifact[16:18], artifact[18:20] = b"\x7fELF\x02\x01", b"\x02\x00", b"\x3e\x00"
        digest = hashlib.sha256(artifact).hexdigest()
        patch_target = "/opt/chain/repairs/chainctl-0.2.66-nginx-log-linux-x86_64-" + digest[:12]
        def runtime(name, native=True):
            pid = next_pid[0]
            next_pid[0] += 10
            pids[name] = pid
            role = name.removeprefix("chain-")
            component = "panel" if role == "maintenance" else role
            executable = "/opt/chain/current-" + component + "/bin/" + ("chainctl" if role == "maintenance" else name)
            if role == "ingress":
                executable = patch_target if path(patch_target).exists() else "/opt/chain/current-agent/bin/chainctl"
            if not native:
                executable = "/usr/bin/python3"
            process = path("/proc/" + str(pid))
            process.mkdir(parents=True)
            process.joinpath("exe").symlink_to(path(executable))
            variable = "CHAIN_AGENT_DIR" if role == "agent" else "CHAIN_DATA_DIR"
            write("/proc/" + str(pid) + "/environ", variable + "=/var/lib/chain-" + role + "\0")
            if kind == "openrc":
                supervisor = pid + 1
                write("/run/supervise-" + name + ".pid", str(supervisor))
                write("/proc/" + str(supervisor) + "/cmdline", "supervise-daemon\0" + name + "\0--start\0")
                write("/proc/" + str(supervisor) + "/task/" + str(supervisor) + "/children", str(pid))
                path("/proc/" + str(supervisor) + "/exe").symlink_to(path("/sbin/supervise-daemon"))
            return pid
        for role in roles:
            for binary in ("chainctl", "chain-" + role):
                write("/opt/chain/current-" + role + "/bin/" + binary, b"\x7fELF", 0o700)
            path("/var/lib/chain-" + role).mkdir(parents=True)
            if kind == "systemd":
                write("/etc/systemd/system/chain-" + role + ".service", '# chain-panel-installer-v1\n[Service]\nEnvironmentFile="/etc/chain-' + role + '/' + role + '.env"\nProtectSystem=strict\n')
                pids["chain-" + role] = 0
            else:
                write("/etc/init.d/chain-" + role, "#!/sbin/openrc-run\n# chain-panel-installer-v1\nsupervisor=supervise-daemon\ncommand='/opt/chain/current-" + role + "/bin/chain-" + role + "'\ncommand_args=''\n", 0o755)
                runtime("chain-" + role)
        if "panel" in roles:
            write("/var/lib/chain-maintenance/.chain-owner", "chain-panel-installer-v1")
            path("/var/lib/chain-maintenance").chmod(0o700)
            args = "-B /opt/chain/current-panel/scripts/chain_maintenance.py"
            if kind == "openrc":
                write("/etc/init.d/chain-maintenance", "#!/sbin/openrc-run\n# chain-panel-installer-v1\nsupervisor=supervise-daemon\ncommand=/usr/bin/python3\ncommand_args=" + namespace["shlex"].quote(args) + "\nexport PYTHONDONTWRITEBYTECODE=1\n", 0o755)
            else:
                write("/etc/systemd/system/chain-maintenance.service", "# chain-panel-installer-v1\n[Service]\nExecStart=/usr/bin/python3 " + args + "\n")
                write("/etc/systemd/system/chain-maintenance.socket", "# chain-panel-installer-v1\n[Socket]\nListenStream=/run/chain-maintenance/control.sock\n")
            runtime("chain-maintenance", native=False)
        if missing_ingress or enabled_ingress:
            write("/var/lib/chain-ingress/state.json", "{}")
        if enabled_ingress:
            write("/var/lib/chain-agent/ingress-request.json", '{"id":"old"}')
            write("/var/lib/chain-ingress/status.json", '{"id":"old","applied":true}')
            write("/etc/nginx/nginx.conf", "events {}\nhttp {}\n")
            if kind == "systemd":
                write("/etc/systemd/system/chain-ingress.service", '# chain-ingress managed v1\n[Service]\nExecStart="/opt/chain/current-agent/bin/chainctl" internal-ingress-apply\n')
                write("/etc/systemd/system/chain-ingress.path", "# chain-ingress managed v1\n[Path]\nPathChanged=/var/lib/chain-agent/ingress-request.json\n")
            else:
                write("/etc/init.d/chain-ingress", "#!/sbin/openrc-run\n# chain-ingress managed v1\nsupervisor=supervise-daemon\ncommand=/opt/chain/current-agent/bin/chainctl\ncommand_args=internal-ingress-watch\n", 0o755)
                runtime("chain-ingress")
        snapshot = {str(p): p.read_bytes() for p in root.rglob("*") if p.is_file() and not p.is_symlink()}
        def new_request():
            request_counter[0] += 1
            request_id = "new-" + str(request_counter[0])
            write("/var/lib/chain-agent/ingress-request.json", json.dumps({"id": request_id}))
            write("/var/lib/chain-ingress/status.json", json.dumps({"id": request_id, "applied": not fresh_error[0], "error": "fresh native failure" if fresh_error[0] else ""}))
        def fake_run(argv, **kwargs):
            args = tuple(argv)
            calls.append(args)
            output = ""
            if args[0] == "systemctl" and args[1] == "show":
                unit, prop = args[2], args[4]
                name = unit.removesuffix(".service")
                output = {"LoadState": "loaded", "DropInPaths": "", "ActiveState": "inactive" if unit == "chain-ingress.service" else "active", "MainPID": str(pids.get(name, 0)),
                          "EnvironmentFiles": "/etc/" + name + "/" + name.removeprefix("chain-") + ".env (ignore_errors=no)"}[prop]
            elif args[0] in ("systemctl", "rc-service"):
                action, name = (args[1], args[2] if len(args) > 2 else "") if args[0] == "systemctl" else (args[2], args[1])
                name = name.removesuffix(".service")
                if action == "stop" and name in pids:
                    pid = pids[name]
                    if pid:
                        shutil.rmtree(path("/proc/" + str(pid)))
                        if kind == "openrc":
                            shutil.rmtree(path("/proc/" + str(pid + 1)))
                            path("/run/supervise-" + name + ".pid").unlink()
                    pids[name] = 0
                elif action == "start" and name in pids:
                    runtime(name)
                    if name == "chain-agent" and enabled_ingress:
                        if delayed_download:
                            pending_request[0] = True
                        else:
                            new_request()
                    if name == "chain-maintenance":
                        sockpath = path("/run/chain-maintenance/control.sock")
                        sockpath.parent.mkdir(parents=True, exist_ok=True)
                        sock = socket.socket(socket.AF_UNIX)
                        sock.bind(str(sockpath))
                        sockets.append(sock)
                else:
                    assert action in ("stop", "start", "reset-failed", "daemon-reload"), args
            elif args == ("/usr/sbin/nginx", "-V"):
                return subprocess.CompletedProcess(args, 0, "", "configure arguments: --prefix=/usr/share/nginx --conf-path=/etc/nginx/nginx.conf")
            else:
                raise AssertionError("unexpected external command: " + str(args))
            return subprocess.CompletedProcess(args, 0, output, "")
        def download(url, timeout):
            assert enabled_ingress, "no-ingress host attempted network access"
            assert url == namespace["PATCH_URL"] and timeout == 30
            downloads.append(url)
            if delayed_download:
                clock[0] += 83
                write("/var/lib/chain-agent/ingress-request.json", '{"id":"during-download"}')
                write("/var/lib/chain-ingress/status.json", '{"id":"during-download","applied":false,"error":"old release chainctl: log format is not specified"}')
            response = io.BytesIO(bytes(artifact) + (b"wrong digest" if bad_download else b""))
            response.headers = {}
            return response
        original_stat = Path.stat
        original_lstat = Path.lstat
        class RootStat:
            st_uid = st_gid = 0
            def __init__(self, value): self.value = value
            def __getattr__(self, name): return getattr(self.value, name)
        def root_stat(self, *args, **kwargs): return RootStat(original_stat(self, *args, **kwargs))
        def root_lstat(self): return RootStat(original_lstat(self))
        def sleep(seconds):
            clock[0] += seconds
            if pending_request[0]:
                new_request()
                pending_request[0] = False
        try:
            with patch.dict(namespace, {"Path": path, "PATCH_SHA256": digest}), patch.object(Path, "stat", root_stat), patch.object(Path, "lstat", root_lstat), patch.object(os, "geteuid", return_value=0), patch.object(os, "uname", return_value=SimpleNamespace(machine="x86_64")), patch.object(namespace["subprocess"], "run", fake_run), patch.object(namespace["shutil"], "which", side_effect=lambda name: "/usr/sbin/nginx" if name == "nginx" else "/sbin/supervise-daemon"), patch.object(namespace["urllib"].request, "build_opener", return_value=SimpleNamespace(open=download)), patch.object(namespace["time"], "monotonic", side_effect=lambda: clock[0]), patch.object(namespace["time"], "sleep", sleep), contextlib.redirect_stdout(io.StringIO()):
                if missing_ingress:
                    try:
                        namespace["main"]()
                    except RuntimeError as error:
                        assert "服务文件缺失" in str(error)
                    else:
                        raise AssertionError("accepted missing enabled-ingress unit")
                    assert snapshot == {str(p): p.read_bytes() for p in root.rglob("*") if p.is_file() and not p.is_symlink()}
                    assert not path("/var/backups").exists()
                elif bad_download:
                    try:
                        namespace["main"]()
                    except RuntimeError as error:
                        assert "SHA256 不匹配" in str(error), str(error)
                    else:
                        raise AssertionError("accepted bad download checksum")
                    assert not any("stop" in call or "start" in call for call in calls), calls
                    assert not path("/opt/chain/repairs").exists()
                    for name, data in snapshot.items():
                        assert Path(name).read_bytes() == data, name
                else:
                    if fresh_failure:
                        try:
                            namespace["main"]()
                        except RuntimeError as error:
                            assert "fresh native failure" in str(error), str(error)
                        else:
                            raise AssertionError("accepted a genuine new-request failure")
                        fresh_error[0] = False
                    namespace["main"]()
                    if delayed_download:
                        assert request_counter[0] >= 1 and clock[0] >= 103
                    if not enabled_ingress:
                        assert not path("/var/lib/chain-ingress").exists()
                        assert not path("/etc/nginx").exists()
                        assert all("nginx" not in arg for call in calls for arg in call)
                        assert not downloads
                    else:
                        assert path(patch_target).read_bytes() == artifact
                        assert path("/opt/chain/current-agent/bin/chainctl").read_bytes() == b"\x7fELF"
                        assert len(downloads) == 1
                    if roles == ["agent"]:
                        assert not path("/var/lib/chain-panel").exists()
                        assert all("chain-panel" not in arg and "chain-maintenance" not in arg for call in calls for arg in call)
                    first_pids = dict(pids)
                    calls.clear()
                    namespace["main"]()
                    assert pids == first_pids
                    assert all(call[1] in ("show", "-V") for call in calls), calls
                    assert len(downloads) == (1 if enabled_ingress else 0)
        finally:
            for sock in sockets:
                sock.close()
    assert not Path(directory).exists()


fixture("systemd", ["agent"])
fixture("systemd", ["agent"], missing_ingress=True)
fixture("openrc", ["panel"])
fixture("systemd", ["agent"], enabled_ingress=True, bad_download=True)
fixture("systemd", ["agent"], enabled_ingress=True)
fixture("openrc", ["agent"], enabled_ingress=True)
fixture("openrc", ["agent"], enabled_ingress=True, delayed_download=True)
fixture("systemd", ["agent"], enabled_ingress=True, delayed_download=True, fresh_failure=True)
print("PASS: role/init migration, optional ingress, pinned artifact, long-download request race, genuine new failure/recovery and no-op reruns")
