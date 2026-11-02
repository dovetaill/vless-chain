#!/bin/sh
# Repair existing 0.2.66 systemd/OpenRC deployments without installing components.
set -eu
command -v python3 >/dev/null 2>&1 || {
    echo '修复脚本需要已有 python3；未修改系统，也不会自动安装依赖。' >&2
    exit 1
}
exec python3 -B - <<'PY'
import fcntl
import hashlib
import json
import os
from pathlib import Path
import re
import shlex
import shutil
import stat
import subprocess
import sys
import tempfile
import time
import urllib.request

AUDIT = None
PATCH_SHA256 = "d60fd0307d6c16fd5b9552ea3e2dce09f3c6617b8cf491ee1f36e5859d44fe70"
PATCH_URL = "https://chain.bareai.io/repairs/chainctl-0.2.66-nginx-log-linux-x86_64"
PATCH_LIMIT = 64 * 1024 * 1024


def require(ok, message):
    if not ok:
        raise RuntimeError(message)


def patch_binary():
    return "/opt/chain/repairs/chainctl-0.2.66-nginx-log-linux-x86_64-" + PATCH_SHA256[:12]


def unit_text(text, role, ingress_binary=None):
    owned_text(text, role)
    component = "agent" if role == "ingress" else "panel"
    action = "internal-ingress-apply" if role == "ingress" else "maintenance"
    binary = "/opt/chain/current-" + component + "/bin/chainctl"
    target = ingress_binary if role == "ingress" and ingress_binary else binary
    require(target in (binary, patch_binary()), "不受信的入口修复路径")
    matches = list(re.finditer(r"(?m)^ExecStart=(.*)$", text))
    require(len(matches) == 1, "服务启动命令不是唯一一条：" + role)
    match = matches[0]
    command = shlex.split(match.group(1))
    legacy = ["-B", "/opt/chain/current-" + component + "/scripts/chain_" + role + ".py"]
    if role == "ingress":
        legacy.append("apply")
    if command == [target, action]:
        return text
    require(command == [binary, action] or (command and re.fullmatch(r"/usr/(?:local/)?bin/python3(?:\.\d+)?", command[0])
            and command[1:] == legacy), "拒绝覆盖自定义启动命令：" + role)
    return text[:match.start()] + 'ExecStart="' + target + '" ' + action + text[match.end():]


def environment_text(text, role):
    require("chain-panel-installer-v1" in text, "服务文件归属不匹配：" + role)
    matches = list(re.finditer(r"(?m)^EnvironmentFile=(.*)$", text))
    require(len(matches) == 1, "环境文件不是唯一一条：" + role)
    match = matches[0]
    path = "/etc/chain-" + role + "/" + role + ".env"
    require(match.group(1) in (path, '"' + path + '"'), "拒绝覆盖自定义环境文件：" + role)
    return text[:match.start()] + "EnvironmentFile=" + path + text[match.end():]


def owned_text(text, role):
    markers = ("# chain-ingress managed v1", "chain-panel-installer-v1") if role == "ingress" else ("chain-panel-installer-v1",)
    require(any(marker in text for marker in markers), "服务文件归属不匹配：" + role)


def openrc_text(text, role, ingress_binary=None):
    owned_text(text, role)
    require(text.startswith("#!/sbin/openrc-run\n") and "supervisor=supervise-daemon" in text,
            "非标准 OpenRC 服务：" + role)
    values, matches = {}, {}
    for name in ("command", "command_args"):
        found = list(re.finditer(r"(?m)^" + name + r"=(.*)$", text))
        require(len(found) == 1, "OpenRC 启动配置不唯一：" + role + "/" + name)
        parsed = shlex.split(found[0].group(1))
        require(len(parsed) == 1, "OpenRC 启动配置不是字面值：" + role + "/" + name)
        values[name], matches[name] = parsed[0], found[0]
    args = shlex.split(values["command_args"])
    component = "agent" if role == "ingress" else "panel" if role == "maintenance" else role
    binary = "/opt/chain/current-" + component + "/bin/" + ("chainctl" if role in ("ingress", "maintenance") else "chain-" + role)
    target = ingress_binary if role == "ingress" and ingress_binary else binary
    require(target in (binary, patch_binary()), "不受信的入口修复路径")
    action = ["internal-ingress-watch"] if role == "ingress" else ["maintenance"] if role == "maintenance" else []
    if values["command"] == target and args == action:
        return text
    require(role in ("ingress", "maintenance"), "拒绝覆盖自定义 OpenRC 主程序：" + role)
    legacy = ["-B", "/opt/chain/current-" + component + "/scripts/chain_" + role + ".py"]
    if role == "ingress":
        legacy.append("watch")
    require((values["command"] == binary and args == action)
            or (re.fullmatch(r"/usr/(?:local/)?bin/python3(?:\.\d+)?", values["command"])
                and args == legacy), "拒绝覆盖自定义 OpenRC 启动命令：" + role)
    changes = {"command": shlex.quote(target), "command_args": shlex.quote(shlex.join(action))}
    for name, match in sorted(matches.items(), key=lambda item: item[1].start(), reverse=True):
        text = text[:match.start()] + name + "=" + changes[name] + text[match.end():]
    return text


def installed_roles(state):
    require(state.get("owner") == "chain-panel-installer-v1", "安装记录归属不匹配")
    roles = state.get("roles")
    require(isinstance(roles, dict) and roles and set(roles) <= {"agent", "panel"}, "安装角色记录无效")
    require(all(isinstance(value, dict) and value.get("version") == "0.2.66" for value in roles.values()),
            "请先将已安装组件升级到 0.2.66；此脚本仅修复迁移，不下载安装组件")
    return [role for role in ("panel", "agent") if role in roles]


def module_text(text, prefix):
    def replace(match):
        module = (prefix / "modules" / match.group(2)).resolve(strict=True)
        require(module.is_file() and module.stat().st_uid == 0
                and not module.stat().st_mode & 0o022, "Nginx 模块归属异常：" + str(module))
        require(not any(c in str(module) for c in '\n\r"\\'), "Nginx 模块路径无效")
        return 'load_module "' + str(module) + '";'
    return re.sub(r'(?m)^[ \t]*load_module[ \t]+([\'\"]?)modules/([A-Za-z0-9_.-]+\.so)\1[ \t]*;[ \t]*$', replace, text)


def command(*args, check=True):
    if AUDIT:
        with AUDIT.open("a") as file:
            file.write(json.dumps({"at": time.time(), "argv": args}) + "\n")
    result = subprocess.run(args, capture_output=True, text=True, timeout=70)
    require(not check or result.returncode == 0, "命令失败：" + " ".join(args) + "\n" + result.stderr.strip() + result.stdout.strip())
    return result


def systemctl(*args):
    return command("systemctl", *args).stdout.strip()


def property_value(unit, name):
    return systemctl("show", unit, "-p", name, "--value")


def service_pid(kind, name):
    if kind == "systemd":
        return int(property_value(name + ".service", "MainPID") or "0")
    pidfile = Path("/run/supervise-" + name + ".pid")
    if not pidfile.exists():
        return 0
    checked_file(pidfile)
    pid = pidfile.read_text().strip()
    require(pid.isdecimal() and int(pid) > 1, "OpenRC supervisor PID 无效：" + name)
    process = Path("/proc") / pid
    if not process.exists():
        return 0
    args = process.joinpath("cmdline").read_bytes().rstrip(b"\0").split(b"\0")
    supervisor = shutil.which("supervise-daemon")
    require(supervisor and process.joinpath("exe").resolve() == Path(supervisor).resolve()
            and args[1:3] == [name.encode(), b"--start"], "OpenRC supervisor 归属不符：" + name)
    children = process.joinpath("task", pid, "children").read_text().split()
    require(len(children) <= 1, "OpenRC supervisor 子进程不明确：" + name)
    return int(children[0]) if children else 0


def process_is(pid, binary):
    return pid > 1 and Path("/proc").joinpath(str(pid), "exe").resolve() == Path(binary).resolve()


def process_environment(pid, key):
    if pid <= 1:
        return None
    try:
        values = Path("/proc").joinpath(str(pid), "environ").read_bytes().split(b"\0")
        return next((value.split(b"=", 1)[1].decode() for value in values if value.startswith(key.encode() + b"=")), None)
    except FileNotFoundError:
        return None


def service(kind, action, name):
    if kind == "systemd":
        systemctl(action, name + ".service")
    else:
        command("rc-service", name, action)
        if action == "stop":
            pidfile = Path("/run/supervise-" + name + ".pid")
            if pidfile.exists():
                pid = pidfile.read_text().strip()
                require(pid.isdecimal() and not Path("/proc").joinpath(pid).exists(),
                        "OpenRC supervisor 未退出，请核对 " + str(pidfile) + " 中 PID " + pid)


def checked_file(path):
    info = path.lstat()
    require(stat.S_ISREG(info.st_mode) and info.st_nlink == 1 and info.st_uid == 0
            and not info.st_mode & 0o022, "文件类型或权限异常：" + str(path))
    return info


def no_jobs(directory):
    for path in directory.glob("*.json"):
        require(json.loads(path.read_text()).get("state") not in ("queued", "running"), "维护任务尚未结束：" + path.name)


def patch_directory():
    directory = Path("/opt/chain/repairs")
    for path in (Path("/opt"), Path("/opt/chain"), directory):
        if path.exists() or path.is_symlink():
            info = path.lstat()
            require(stat.S_ISDIR(info.st_mode) and info.st_uid == 0 and not info.st_mode & 0o022,
                    "修复程序目录类型或归属异常：" + str(path))
    return directory


def verify_patch(path):
    info = checked_file(path)
    require(64 <= info.st_size <= PATCH_LIMIT, "修复程序大小异常")
    digest = hashlib.sha256()
    with path.open("rb") as file:
        header = file.read(64)
        require(header[:6] == b"\x7fELF\x02\x01" and header[18:20] == b"\x3e\x00"
                and header[16:18] in (b"\x02\x00", b"\x03\x00"), "修复程序不是 Linux x86_64 ELF")
        digest.update(header)
        for chunk in iter(lambda: file.read(1024 * 1024), b""):
            digest.update(chunk)
    require(digest.hexdigest() == PATCH_SHA256, "修复程序 SHA256 不匹配，未停服务")


class HTTPSRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, request, fp, code, msg, headers, newurl):
        require(newurl.startswith("https://"), "拒绝非 HTTPS 下载重定向")
        return super().redirect_request(request, fp, code, msg, headers, newurl)


def install_patch():
    require(re.fullmatch(r"[0-9a-f]{64}", PATCH_SHA256), "原生修复程序尚未构建完成；未停服务")
    directory = patch_directory()
    destination = Path(patch_binary())
    if destination.exists() or destination.is_symlink():
        verify_patch(destination)
        require(os.access(destination, os.X_OK), "已有修复程序不可执行；未修改其权限")
        return
    created = not directory.exists()
    directory.mkdir(mode=0o755, exist_ok=True)
    temporary = None
    try:
        fd, name = tempfile.mkstemp(prefix=".chainctl-download-", dir=directory)
        temporary = Path(name)
        with os.fdopen(fd, "wb") as file:
            with urllib.request.build_opener(HTTPSRedirect()).open(PATCH_URL, timeout=30) as response:
                length = response.headers.get("Content-Length")
                require(length is None or int(length) <= PATCH_LIMIT, "下载程序超过 64 MiB")
                size, start = 0, time.monotonic()
                while True:
                    require(time.monotonic() - start <= 180, "下载修复程序超时")
                    chunk = response.read(1024 * 1024)
                    if not chunk:
                        break
                    size += len(chunk)
                    require(size <= PATCH_LIMIT, "下载程序超过 64 MiB")
                    file.write(chunk)
            file.flush()
            os.fsync(file.fileno())
            verify_patch(temporary)
            os.fchmod(file.fileno(), 0o755)
        os.link(temporary, destination, follow_symlinks=False)
        print("已验证并安装独立入口修复程序：" + str(destination), flush=True)
    finally:
        if temporary is not None:
            temporary.unlink(missing_ok=True)
        if created and not destination.exists():
            directory.rmdir()


def metadata(path):
    info = path.lstat()
    return dict(path=str(path), realpath=str(path.resolve()), dev=info.st_dev,
                inode=info.st_ino, uid=info.st_uid, gid=info.st_gid,
                mode=stat.S_IMODE(info.st_mode), ctime_ns=info.st_ctime_ns,
                xattrs={name: os.getxattr(path, name, follow_symlinks=False).hex()
                        for name in os.listxattr(path, follow_symlinks=False)})


def nginx_files():
    import glob
    binary = shutil.which("nginx")
    require(binary, "已启用统一入口，但未找到 nginx")
    version = command(binary, "-V").stderr
    options = dict(item[2:].split("=", 1) for item in shlex.split(version)
                   if item.startswith("--") and "=" in item)
    require("prefix" in options, "无法确认 Nginx 编译前缀")
    prefix = Path(options["prefix"])
    main = Path(options.get("conf-path", str(prefix / "conf/nginx.conf")))
    pending, seen, result = [main], set(), {}
    while pending:
        source = pending.pop()
        path = source.resolve(strict=True)
        if path in seen:
            continue
        seen.add(path)
        require(len(seen) <= 256, "Nginx include 超过检查范围")
        text = path.read_text()
        for raw in re.findall(r"(?m)^[ \t]*include[ \t]+([^;\n]+);", text):
            args = shlex.split(raw)
            require(len(args) == 1 and "$" not in args[0], "Nginx include 路径无法静态确认")
            include = Path(args[0])
            if not include.is_absolute():
                include = main.parent / include
            pending.extend(Path(name) for name in glob.glob(str(include)))
        new = module_text(text, prefix)
        if new != text:
            checked_file(path)
            result[path] = (text.encode(), new.encode())
    return result, main


def main():
    global AUDIT
    AUDIT = None
    require(os.geteuid() == 0, "请使用 sudo sh 或 root 执行")
    kind = "systemd" if Path("/run/systemd/system").is_dir() else "openrc" if Path("/run/openrc").exists() else None
    require(kind, "仅支持正在运行的 Linux systemd 或 OpenRC")
    roles = installed_roles(json.loads(Path("/opt/chain/.chain-install.json").read_text()))
    for role in roles:
        binary = Path("/opt/chain/current-" + role + "/bin/chainctl")
        with binary.open("rb") as file:
            require(file.read(4) == b"\x7fELF" and os.access(binary, os.X_OK), "缺少原生 chainctl：" + str(binary))
    ingress = Path("/var/lib/chain-ingress/state.json").exists()
    require(not ingress or "agent" in roles, "存在统一入口状态，但安装记录没有 Agent")
    use_patch = ingress and os.uname().machine == "x86_64"
    ingress_binary = patch_binary() if use_patch else "/opt/chain/current-agent/bin/chainctl"
    names = roles + (["maintenance"] if "panel" in roles else []) + (["ingress"] if ingress else [])
    base = Path("/etc/systemd/system") if kind == "systemd" else Path("/etc/init.d")
    paths, updates = {}, {}
    for name in names:
        path = base / ("chain-" + name + (".service" if kind == "systemd" else ""))
        require(path.exists(), "已安装功能的服务文件缺失，未创建或启用功能：" + str(path))
        checked_file(path)
        text = path.read_text()
        transform = openrc_text if kind == "openrc" else environment_text if name in roles else unit_text
        new = transform(text, name, ingress_binary) if name == "ingress" else transform(text, name)
        paths[name] = path
        if new != text:
            updates[path] = (text.encode(), new.encode())
        if kind == "openrc":
            override = Path("/etc/conf.d/chain-" + name)
            require(not override.exists() or not override.read_text().strip(), "发现 OpenRC 自定义覆盖，未修改：" + str(override))
    auxiliary = (["chain-maintenance.socket"] if "panel" in roles else []) + (["chain-ingress.path"] if ingress else [])
    if kind == "systemd":
        for unit in ["chain-" + name + ".service" for name in names] + auxiliary:
            require(property_value(unit, "LoadState") == "loaded", "服务未正确加载：" + unit)
            require(not property_value(unit, "DropInPaths"), "发现自定义服务覆盖，请先核对：" + unit)
        for unit in auxiliary:
            path = base / unit
            checked_file(path)
            owned_text(path.read_text(), "ingress" if unit == "chain-ingress.path" else "maintenance")
        if ingress:
            text = base.joinpath("chain-ingress.path").read_text()
            require(re.search(r"(?m)^PathChanged=/var/lib/chain-agent/ingress-request.json$", text), "入口监听路径非标准")
            require(property_value("chain-ingress.service", "ActiveState") in ("inactive", "failed"), "入口任务正在执行，请稍后重试")
    maintenance = Path("/var/lib/chain-maintenance")
    if "panel" in roles:
        require(maintenance.joinpath(".chain-owner").read_text() == "chain-panel-installer-v1", "维护目录归属不匹配")
        info = maintenance.stat()
        require((info.st_uid, info.st_gid, stat.S_IMODE(info.st_mode)) == (0, 0, 0o700), "维护目录权限异常；未修改权限")
        no_jobs(maintenance)
    patch_missing = False
    if use_patch:
        require(re.fullmatch(r"[0-9a-f]{64}", PATCH_SHA256), "原生修复程序尚未构建完成；未停服务")
        patch_directory()
        artifact = Path(ingress_binary)
        patch_missing = not artifact.exists()
        if artifact.exists() or artifact.is_symlink():
            verify_patch(artifact)
            require(os.access(artifact, os.X_OK), "已有修复程序不可执行；未修改其权限")
    elif ingress:
        status_file = Path("/var/lib/chain-ingress/status.json")
        status = json.loads(status_file.read_text()) if status_file.exists() else {}
        require("log format is not specified" not in str(status.get("error", "")),
                "此架构暂缺 Nginx 日志校验原生补丁；未停服务，未改用户日志配置")
    nginx_updates, nginx_main = nginx_files() if ingress else ({}, None)
    updates.update(nginx_updates)
    binary = {name: "/opt/chain/current-" + ("agent" if name == "ingress" else "panel" if name == "maintenance" else name)
              + "/bin/" + ("chainctl" if name in ("ingress", "maintenance") else "chain-" + name) for name in names}
    if ingress:
        binary["ingress"] = ingress_binary
    pids = {name: service_pid(kind, "chain-" + name) for name in names if name != "ingress" or kind == "openrc"}
    restart = {name for name, pid in pids.items() if not process_is(pid, binary[name])}
    for role in roles:
        variable = "CHAIN_AGENT_DIR" if role == "agent" else "CHAIN_DATA_DIR"
        if process_environment(pids[role], variable) != "/var/lib/chain-" + role:
            restart.add(role)
    if "maintenance" in paths and paths["maintenance"] in updates:
        restart.add("maintenance")
    if "maintenance" in paths:
        socket = Path("/run/chain-maintenance/control.sock")
        if not socket.exists() or not stat.S_ISSOCK(socket.stat().st_mode):
            restart.add("maintenance")
        if kind == "systemd" and property_value("chain-maintenance.socket", "ActiveState") != "active":
            restart.add("maintenance")
    request_path = Path("/var/lib/chain-agent/ingress-request.json")
    status_path = Path("/var/lib/chain-ingress/status.json")
    old_request = None
    ingress_restart = False
    if ingress:
        old_request = json.loads(request_path.read_text()).get("id") if request_path.exists() else None
        status = json.loads(status_path.read_text()) if status_path.exists() else {}
        ingress_restart = (paths["ingress"] in updates or bool(nginx_updates) or "ingress" in restart or patch_missing
                           or status.get("applied") is not True or status.get("id") != old_request)
        if kind == "systemd" and property_value("chain-ingress.path", "ActiveState") != "active":
            ingress_restart = True
        if ingress_restart:
            restart.add("agent")
    if not updates and not restart and not ingress_restart:
        print("已安装组件的服务入口与运行程序符合修复条件，无需修改或重启；未创建备份。")
        return
    lock = os.open(Path("/opt/chain/.install.lock"), os.O_RDWR | os.O_NOFOLLOW)
    fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
    try:
        if "panel" in roles:
            no_jobs(maintenance)
    except Exception:
        os.close(lock)
        raise
    watched = set(paths.values()) | set(updates) | {Path("/var/lib/chain-" + role) for role in roles}
    if "panel" in roles:
        watched.add(maintenance)
    if ingress:
        watched.update((Path("/var/lib/chain-ingress"), Path("/etc/chain-ingress/routes.conf"), nginx_main, Path("/var/lib/nginx")))
        watched.update(Path("/var/lib/nginx").glob("*"))
    if use_patch:
        watched.update((Path("/opt/chain"), Path("/opt/chain/repairs"), Path(ingress_binary)))
    before = {str(path): metadata(path) for path in watched if path.exists()}
    backup_parent = Path("/var/backups")
    require(not backup_parent.is_symlink(), "备份父目录不能是符号链接")
    backup_parent.mkdir(mode=0o755, exist_ok=True)
    backup = Path(tempfile.mkdtemp(prefix="chain-repair-", dir=backup_parent))
    backup.joinpath("before.json").write_text(json.dumps(before, indent=2))
    backup.joinpath("identity.json").write_text(json.dumps({"uid": os.geteuid(), "gid": os.getegid(), "groups": os.getgroups(), "init": kind, "roles": roles, "processes": pids, "at": time.time()}))
    for index, (path, values) in enumerate(updates.items()):
        backup.joinpath(str(index) + "-" + path.name).write_bytes(values[0])
    AUDIT = backup / "commands.jsonl"
    print("原文件与元数据已备份：" + str(backup), flush=True)
    try:
        if use_patch:
            install_patch()
            backup.joinpath("artifact.json").write_text(json.dumps({"path": ingress_binary, "sha256": PATCH_SHA256, "url": PATCH_URL}))
            if "panel" in roles:
                no_jobs(maintenance)
        for name in ("maintenance", "agent", "panel"):
            if name in restart:
                if name == "maintenance" and kind == "systemd":
                    systemctl("stop", "chain-maintenance.socket")
                service(kind, "stop", "chain-" + name)
                require(service_pid(kind, "chain-" + name) == 0, "旧服务进程尚未退出：" + name)
        if ingress_restart:
            if kind == "systemd":
                systemctl("stop", "chain-ingress.path", "chain-ingress.service")
            else:
                service(kind, "stop", "chain-ingress")
                require(service_pid(kind, "chain-ingress") == 0, "旧入口进程尚未退出")
        if ingress and "agent" in restart:
            # Ignore requests emitted by the old Agent while the patch was downloading.
            old_request = json.loads(request_path.read_text()).get("id") if request_path.exists() else None
        for path, (original, replacement) in updates.items():
            with open(os.open(path, os.O_RDWR | os.O_NOFOLLOW), "r+b") as file:
                info = os.fstat(file.fileno())
                require((info.st_dev, info.st_ino) == (before[str(path)]["dev"], before[str(path)]["inode"])
                        and file.read() == original, "文件被并行修改，已停止：" + str(path))
                file.seek(0)
                file.write(replacement)
                file.truncate()
                file.flush()
                os.fsync(file.fileno())
        if kind == "systemd":
            systemctl("daemon-reload")
            for role in roles:
                expected = "/etc/chain-" + role + "/" + role + ".env"
                require(expected in property_value("chain-" + role + ".service", "EnvironmentFiles"), "systemd 未加载环境文件：" + role)
            for name in restart:
                systemctl("reset-failed", "chain-" + name + ".service")
            if ingress_restart:
                systemctl("reset-failed", "chain-ingress.service", "chain-ingress.path")
                systemctl("start", "chain-ingress.path")
        elif ingress_restart:
            service(kind, "start", "chain-ingress")
        for name in ("panel", "maintenance", "agent"):
            if name in restart:
                if name == "maintenance" and kind == "systemd":
                    systemctl("start", "chain-maintenance.socket")
                service(kind, "start", "chain-" + name)
        deadline = time.monotonic() + 60
        stable_since, last_pids = None, None
        while time.monotonic() < deadline:
            current = {name: service_pid(kind, "chain-" + name) for name in pids}
            healthy = all(process_is(pid, binary[name]) for name, pid in current.items())
            healthy = healthy and all(process_environment(current[role], "CHAIN_AGENT_DIR" if role == "agent" else "CHAIN_DATA_DIR") == "/var/lib/chain-" + role for role in roles)
            applied = True
            if ingress and (ingress_restart or "agent" in restart):
                request = json.loads(request_path.read_text()) if request_path.exists() else {}
                status = json.loads(status_path.read_text()) if status_path.exists() else {}
                applied = request.get("id") not in (None, old_request) and status.get("id") == request.get("id")
                if applied:
                    require(status.get("applied") is True, "入口应用失败：" + str(status.get("error", "未知错误")))
            if healthy and current == last_pids:
                if stable_since is not None and time.monotonic() - stable_since >= 20 and applied:
                    break
            else:
                stable_since = time.monotonic() if healthy else None
            last_pids = current
            time.sleep(1)
        else:
            raise RuntimeError("60 秒内未确认服务持续稳定及所需的新入口请求；请检查服务日志")
        if "panel" in roles:
            require(stat.S_ISSOCK(Path("/run/chain-maintenance/control.sock").stat().st_mode), "维护 socket 未就绪")
            if kind == "systemd":
                require(property_value("chain-maintenance.socket", "ActiveState") == "active", "维护 socket 单位未启动")
    finally:
        after = {str(path): metadata(path) for path in watched if path.exists()}
        backup.joinpath("after.json").write_text(json.dumps(after, indent=2))
        changed = [path for path in before if path not in after or any(before[path][key] != after[path][key] for key in ("uid", "gid", "mode", "xattrs"))]
        changed += [str(path) for path in updates if str(path) in after and any(before[str(path)][key] != after[str(path)][key] for key in ("dev", "inode"))]
        os.close(lock)
        require(not changed, "发现文件身份/属主/权限/扩展属性变化，未擅自恢复：" + ", ".join(changed))
        print("相关既有路径属主、属组、权限及扩展属性未变；修改文件保留 inode。审计记录：" + str(backup))
    print("本机服务修复检查通过：实际原生程序与所需环境已确认，连续运行至少 20 秒。")
    print("未执行代理端到端测试；请在面板核对新的 Agent 心跳。后续运行原版安装器可能重新覆盖现场修复。")


if __name__ == "__main__":
    try:
        main()
    except Exception as error:
        print("修复未完成：" + str(error), file=sys.stderr)
        print("请核对报错和服务状态；仅在完成备份后才会输出具体备份路径。", file=sys.stderr)
        sys.exit(1)
PY
