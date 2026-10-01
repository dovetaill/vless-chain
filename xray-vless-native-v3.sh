#!/usr/bin/env bash
set -Eeuo pipefail

# Native VLESS / Xray instance manager for Debian, CentOS and Alpine.
# No Docker. Reuses an existing Nginx installation when available.

APP_DIR="${APP_DIR:-/etc/xray-chain}"
WEB_ROOT="${WEB_ROOT:-/var/www/xray-chain}"
XRAY_BIN="${XRAY_BIN:-/usr/local/bin/xray}"
SERVICE_NAME="${SERVICE_NAME:-xray-chain}"
MANAGER_DIR="${MANAGER_DIR:-/etc/xray-chain-manager}"
# Alpine installations may keep /var/www private (0700). Use a separate public
# tree so Nginx can traverse it without changing permissions on existing sites.
MANAGER_WEB_ROOT="${MANAGER_WEB_ROOT:-/var/lib/xray-chain-manager}"
SYSTEMD_DIR="${SYSTEMD_DIR:-/etc/systemd/system}"
NGINX_CONF_DIR="${NGINX_CONF_DIR:-/etc/nginx/conf.d}"
RENEW_HOOK_DIR="${RENEW_HOOK_DIR:-/etc/letsencrypt/renewal-hooks/deploy}"
OPENRC_DIR="${OPENRC_DIR:-/etc/init.d}"
INIT_SYSTEM="${INIT_SYSTEM:-}"
PACKAGE_MANAGER=""
INSTANCE_ID="${INSTANCE_ID:-}"
INSTANCE_NAME="${INSTANCE_NAME:-}"
INSTANCE_IMPORTED="${INSTANCE_IMPORTED:-false}"

DOMAIN="${DOMAIN:-}"
EMAIL="${EMAIL:-}"
UUID="${UUID:-}"
NODE_MODE="${NODE_MODE:-}"
SECURITY_MODE="${SECURITY_MODE:-}"
CERT_MODE="${CERT_MODE:-}"

TLS_PORT="${TLS_PORT:-}"
REALITY_PORT="${REALITY_PORT:-}"
LISTEN_PORT="${LISTEN_PORT:-}"

# Certificate modes:
#   http       = Certbot HTTP-01 via Nginx (DNS provider independent, port 80 required)
#   cloudflare = Certbot Cloudflare DNS-01
#   existing   = reuse an existing certificate/key
#   none       = only valid when no local TLS certificate is required
CF_API_TOKEN="${CF_API_TOKEN:-}"
CERT_FILE="${CERT_FILE:-}"
KEY_FILE="${KEY_FILE:-}"

REALITY_TARGET_MODE="${REALITY_TARGET_MODE:-local}"
REALITY_TARGET="${REALITY_TARGET:-}"
REALITY_SERVER_NAME="${REALITY_SERVER_NAME:-}"
REALITY_PRIVATE_KEY="${REALITY_PRIVATE_KEY:-}"
REALITY_PUBLIC_KEY="${REALITY_PUBLIC_KEY:-}"
REALITY_SHORT_ID="${REALITY_SHORT_ID:-}"
REALITY_FINGERPRINT="${REALITY_FINGERPRINT:-chrome}"

UPSTREAM_URI="${UPSTREAM_URI:-}"
UPSTREAM_ADDRESS="${UPSTREAM_ADDRESS:-}"
UPSTREAM_PORT="${UPSTREAM_PORT:-}"
UPSTREAM_UUID="${UPSTREAM_UUID:-}"
UPSTREAM_SECURITY="${UPSTREAM_SECURITY:-}"
UPSTREAM_SNI="${UPSTREAM_SNI:-}"
UPSTREAM_FLOW="${UPSTREAM_FLOW:-}"
UPSTREAM_FINGERPRINT="${UPSTREAM_FINGERPRINT:-chrome}"
UPSTREAM_ALLOW_INSECURE="${UPSTREAM_ALLOW_INSECURE:-false}"
UPSTREAM_REALITY_PUBLIC_KEY="${UPSTREAM_REALITY_PUBLIC_KEY:-}"
UPSTREAM_REALITY_SHORT_ID="${UPSTREAM_REALITY_SHORT_ID:-}"
UPSTREAM_REALITY_MLDSA65_VERIFY="${UPSTREAM_REALITY_MLDSA65_VERIFY:-}"
UPSTREAM_TRANSPORT="${UPSTREAM_TRANSPORT:-}"
UPSTREAM_NAME="${UPSTREAM_NAME:-}"
UPSTREAM_XHTTP_PATH="${UPSTREAM_XHTTP_PATH:-}"
UPSTREAM_XHTTP_HOST="${UPSTREAM_XHTTP_HOST:-}"
UPSTREAM_XHTTP_MODE="${UPSTREAM_XHTTP_MODE:-auto}"
UPSTREAM_XHTTP_EXTRA="${UPSTREAM_XHTTP_EXTRA:-}"
[[ -n "$UPSTREAM_XHTTP_EXTRA" ]] || UPSTREAM_XHTTP_EXTRA='{}'
UPSTREAM_ALPN="${UPSTREAM_ALPN:-}"

INTERNAL_HTTP_PORT="${INTERNAL_HTTP_PORT:-18080}"
INTERNAL_HTTPS_PORT="${INTERNAL_HTTPS_PORT:-18443}"

INTERACTIVE=1
[[ -t 0 ]] || INTERACTIVE=0
NGINX_DOMAIN_PREEXISTED=0
WORK_DIR=""
HTTP_PROBE_FILE=""
INSTALL_PENDING=0
INSTALL_APP_CREATED=0
INSTALL_SERVICE_CREATED=0
INSTALL_NGINX_CREATED=0
INSTALL_HOOK_CREATED=0
INSTALL_HTTP_CREATED=0
INSTALL_CF_CHANGED=0
INSTALL_CF_PATH=""
UPDATE_PENDING=0
UPDATE_WAS_ACTIVE=0
DELETE_PENDING=0
DELETE_WAS_ACTIVE=0
DELETE_WAS_ENABLED=0
EXPORT_PATH=""

# Only named fields cross the Bash/Python boundary. Never source imported data.
STATE_FIELDS=(INSTANCE_ID INSTANCE_NAME INSTANCE_IMPORTED APP_DIR WEB_ROOT SERVICE_NAME INIT_SYSTEM
  XRAY_BIN DOMAIN UUID NODE_MODE SECURITY_MODE CERT_MODE CERT_FILE KEY_FILE TLS_PORT
  REALITY_PORT REALITY_TARGET_MODE REALITY_TARGET REALITY_SERVER_NAME REALITY_PRIVATE_KEY
  REALITY_PUBLIC_KEY REALITY_SHORT_ID REALITY_FINGERPRINT INTERNAL_HTTP_PORT INTERNAL_HTTPS_PORT)
UPSTREAM_FIELDS=(UPSTREAM_ADDRESS UPSTREAM_PORT UPSTREAM_UUID UPSTREAM_SECURITY UPSTREAM_SNI
  UPSTREAM_FLOW UPSTREAM_FINGERPRINT UPSTREAM_ALLOW_INSECURE UPSTREAM_REALITY_PUBLIC_KEY
  UPSTREAM_REALITY_SHORT_ID UPSTREAM_REALITY_MLDSA65_VERIFY UPSTREAM_TRANSPORT UPSTREAM_NAME
  UPSTREAM_XHTTP_PATH UPSTREAM_XHTTP_HOST UPSTREAM_XHTTP_MODE UPSTREAM_XHTTP_EXTRA UPSTREAM_ALPN)
export "${STATE_FIELDS[@]}" "${UPSTREAM_FIELDS[@]}" MANAGER_DIR

red() { printf '\033[31m%s\033[0m\n' "$*"; }
green() { printf '\033[32m%s\033[0m\n' "$*"; }
yellow() { printf '\033[33m%s\033[0m\n' "$*"; }
blue() { printf '\033[36m%s\033[0m\n' "$*"; }
die() { red "错误：$*"; exit 1; }

usage() {
  cat <<USAGE
用法：
  bash $0                           交互管理菜单
  bash $0 install direct 租户编号     新增落地 VLESS
  bash $0 install relay 租户编号      新增入口并绑定远端落地
  bash $0 list                       列出实例
  bash $0 manage 租户编号             单实例运维菜单
  bash $0 status|logs|follow|start|stop|restart|enable|disable|links 租户编号
  bash $0 upstream 租户编号           更换入口绑定的落地
  bash $0 edit 租户编号               交互修改租户信息
  bash $0 credentials 租户编号        恢复链接或重置接入凭据
  bash $0 delete 租户编号             确认后删除；导入实例仅取消登记
  bash $0 diagnose|repair-site 租户编号 诊断实例或修复伪装站权限
  bash $0 import 租户编号 [旧目录] [旧服务名]

支持 Debian/Ubuntu（apt）、CentOS/RHEL 系（dnf/yum）、Alpine（apk）。
服务使用 systemd 或 OpenRC。Alpine 首次运行前：apk add bash
不使用 Docker。每个租户独立 UUID、端口、配置和服务，可同机共存。

入口安全模式：SECURITY_MODE=tls/reality/dual，均为 TCP + Vision。
dual 使用两个端口、同一个 UUID，绑定同一个落地。
落地导入：TLS/REALITY + TCP/raw 或 XHTTP；支持 vless://、单个 YAML/JSON。
多行节点最后输入 END；缺少字段进入补填向导。
停止只改变当前运行状态；关闭开机自启只改变自启设置。
默认管理目录：$MANAGER_DIR。旧部署导入保留配置、UUID、目录和服务。

证书方式：CERT_MODE=http/cloudflare/existing/none
  http：Certbot webroot HTTP-01，要求域名解析到本机、公网 80 可达。
  cloudflare：DNS-01，需要 CF_API_TOKEN。
  existing：提供 CERT_FILE、KEY_FILE。
  none：仅限 REALITY + 外部伪装目标。

非交互示例：
  DOMAIN=node.example.com SECURITY_MODE=reality REALITY_TARGET_MODE=remote \\
  REALITY_TARGET=www.example.com:443 REALITY_PORT=8443 \\
  bash $0 install direct landing01

  DOMAIN=edge.example.com SECURITY_MODE=reality REALITY_TARGET_MODE=remote \\
  REALITY_TARGET=www.example.com:443 REALITY_PORT=9443 \\
  UPSTREAM_URI='vless://...' bash $0 install relay tenant01

新建实例不覆盖旧配置。导入示例：
  bash $0 import legacy /etc/xray-chain xray-chain
USAGE
}

need_cmd() { command -v "$1" >/dev/null 2>&1; }

native_helper() {
  python3 - "$@" <<'PY'
import ipaddress
import json
import os
import re
import sys
import tempfile
from pathlib import Path
from urllib.parse import parse_qs, unquote, urlsplit, urlencode, quote

STATE = """INSTANCE_ID INSTANCE_NAME INSTANCE_IMPORTED APP_DIR WEB_ROOT SERVICE_NAME INIT_SYSTEM
XRAY_BIN DOMAIN UUID NODE_MODE SECURITY_MODE CERT_MODE CERT_FILE KEY_FILE TLS_PORT
REALITY_PORT REALITY_TARGET_MODE REALITY_TARGET REALITY_SERVER_NAME REALITY_PRIVATE_KEY
REALITY_PUBLIC_KEY REALITY_SHORT_ID REALITY_FINGERPRINT INTERNAL_HTTP_PORT INTERNAL_HTTPS_PORT""".split()
UP = """UPSTREAM_ADDRESS UPSTREAM_PORT UPSTREAM_UUID UPSTREAM_SECURITY UPSTREAM_SNI
UPSTREAM_FLOW UPSTREAM_FINGERPRINT UPSTREAM_ALLOW_INSECURE UPSTREAM_REALITY_PUBLIC_KEY
UPSTREAM_REALITY_SHORT_ID UPSTREAM_REALITY_MLDSA65_VERIFY UPSTREAM_TRANSPORT UPSTREAM_NAME
UPSTREAM_XHTTP_PATH UPSTREAM_XHTTP_HOST UPSTREAM_XHTTP_MODE UPSTREAM_XHTTP_EXTRA UPSTREAM_ALPN""".split()

def fail(message):
    raise ValueError(message)

def clean(value):
    text = str(value if value is not None else "")
    if any(ord(c) < 32 for c in text):
        fail("节点字段不能包含换行或控制字符")
    return text

def boolean(value):
    if isinstance(value, bool):
        return value
    if str(value).lower() in ("true", "1"):
        return True
    if str(value).lower() in ("false", "0", ""):
        return False
    fail("布尔字段必须为 true/false")

def dump(data):
    print(json.dumps(data, ensure_ascii=False, indent=2))

def read_json(path):
    return json.loads(Path(path).read_text())

def atomic(path, data):
    path = Path(path)
    fd, temp = tempfile.mkstemp(prefix=".state-", dir=path.parent)
    try:
        with os.fdopen(fd, "w") as handle:
            json.dump(data, handle, ensure_ascii=False, indent=2)
            handle.write("\n")
        os.replace(temp, path)
    finally:
        if os.path.exists(temp):
            os.unlink(temp)

def values(data, fields):
    for key in fields:
        value = clean(data.get(key, ""))
        sys.stdout.buffer.write((key + "\0" + value + "\0").encode())

def environment(fields):
    return {key: os.environ.get(key, "") for key in fields}

def parse_node(text):
    data = dict.fromkeys(UP, "")
    data.update(UPSTREAM_FINGERPRINT="chrome", UPSTREAM_ALLOW_INSECURE="false",
                UPSTREAM_XHTTP_MODE="auto", UPSTREAM_XHTTP_EXTRA="{}")
    if text.startswith("vless://"):
        node = urlsplit(text)
        if node.password:
            fail("VLESS 链接不能包含密码段")
        data.update(UPSTREAM_UUID=unquote(node.username or ""),
                    UPSTREAM_ADDRESS=node.hostname or "",
                    UPSTREAM_PORT=str(node.port or ""),
                    UPSTREAM_NAME=unquote(node.fragment),
                    UPSTREAM_TRANSPORT="raw")
        query = parse_qs(node.query, keep_blank_values=True)
        mapping = {"security": "SECURITY", "type": "TRANSPORT", "sni": "SNI",
                   "flow": "FLOW", "fp": "FINGERPRINT", "pbk": "REALITY_PUBLIC_KEY",
                   "sid": "REALITY_SHORT_ID", "pqv": "REALITY_MLDSA65_VERIFY",
                   "path": "XHTTP_PATH", "host": "XHTTP_HOST", "mode": "XHTTP_MODE",
                   "extra": "XHTTP_EXTRA", "alpn": "ALPN"}
        for key, suffix in mapping.items():
            if key in query:
                if len(query[key]) != 1:
                    fail("链接包含重复参数：" + key)
                data["UPSTREAM_" + suffix] = query[key][0]
        if query.get("encryption", ["none"])[0] not in ("", "none"):
            fail("当前仅支持 encryption=none 的落地")
        insecure = query.get("allowInsecure", query.get("insecure", ["false"]))[0]
        data["UPSTREAM_ALLOW_INSECURE"] = str(boolean(insecure)).lower()
    else:
        try:
            node = json.loads(text)
        except json.JSONDecodeError:
            try:
                import yaml
            except ImportError:
                fail("结构化节点需要 python3-yaml，请先安装该依赖")
            try:
                node = yaml.safe_load(text)
            except yaml.YAMLError:
                fail("YAML/JSON 格式不正确；请粘贴完整的单个节点")
        if isinstance(node, dict) and "proxies" in node:
            node = node["proxies"]
        if isinstance(node, list):
            if len(node) != 1:
                fail("一次只能导入一个节点，请粘贴目标节点")
            node = node[0]
        if not isinstance(node, dict):
            fail("请输入 vless:// 链接或单个 YAML/JSON 节点")
        if node.get("type", "vless") != "vless":
            fail("只支持 type: vless 的节点")
        if node.get("encryption", "none") not in ("", "none", None):
            fail("当前仅支持 encryption=none 的落地")
        for option in ("shadow-tls-opts", "restls-opts", "jls-opts", "ech-opts",
                       "certificate", "private-key", "name-cert-verify"):
            if node.get(option):
                fail("当前不能转换节点参数：" + option)
        if isinstance(node.get("smux"), dict) and node["smux"].get("enabled"):
            fail("当前不能转换 smux，请使用不依赖 smux 的落地配置")
        reality = node.get("reality-opts") or {}
        if not isinstance(reality, dict):
            fail("reality-opts 必须是对象")
        data.update(UPSTREAM_NAME=node.get("name", ""), UPSTREAM_ADDRESS=node.get("server", ""),
                    UPSTREAM_PORT=node.get("port", ""), UPSTREAM_UUID=node.get("uuid", ""),
                    UPSTREAM_SECURITY="reality" if reality else (
                        "tls" if boolean(node.get("tls", False)) else ""),
                    UPSTREAM_TRANSPORT=node.get("network", ""),
                    UPSTREAM_FLOW=node.get("flow", ""),
                    UPSTREAM_SNI=node.get("servername", node.get("sni", "")),
                    UPSTREAM_FINGERPRINT=node.get("client-fingerprint") or "chrome",
                    UPSTREAM_ALLOW_INSECURE=str(boolean(node.get("skip-cert-verify", False))).lower(),
                    UPSTREAM_REALITY_PUBLIC_KEY=reality.get("public-key", ""),
                    UPSTREAM_REALITY_SHORT_ID=reality.get("short-id", ""),
                    UPSTREAM_REALITY_MLDSA65_VERIFY=reality.get("mldsa65-verify", ""))
        alpn = node.get("alpn", [])
        data["UPSTREAM_ALPN"] = ",".join(alpn) if isinstance(alpn, list) else alpn
        xhttp = node.get("xhttp-opts") or {}
        if not isinstance(xhttp, dict):
            fail("xhttp-opts 必须是对象")
        if xhttp:
            data["UPSTREAM_TRANSPORT"] = node.get("network", "xhttp")
            data.update(UPSTREAM_XHTTP_PATH=xhttp.get("path", ""),
                        UPSTREAM_XHTTP_HOST=xhttp.get("host", ""),
                        UPSTREAM_XHTTP_MODE=xhttp.get("mode", "auto"))
            extra = {}
            translated = {"headers": "headers", "no-grpc-header": "noGRPCHeader",
                          "no-sse-header": "noSSEHeader", "x-padding-bytes": "xPaddingBytes",
                          "sc-max-each-post-bytes": "scMaxEachPostBytes",
                          "sc-min-posts-interval-ms": "scMinPostsIntervalMs"}
            for key, value in xhttp.items():
                if key in ("path", "host", "mode"):
                    continue
                if key in translated:
                    extra[translated[key]] = value
                elif key == "reuse-settings":
                    if not isinstance(value, dict):
                        fail("reuse-settings 必须是对象")
                    xmux = {"max-concurrency": "maxConcurrency", "max-connections": "maxConnections",
                            "c-max-reuse-times": "cMaxReuseTimes", "h-max-request-times": "hMaxRequestTimes",
                            "h-max-reusable-secs": "hMaxReusableSecs", "h-keep-alive-period": "hKeepAlivePeriod"}
                    if any(k not in xmux for k in value):
                        fail("存在不能转换的 reuse-settings 参数")
                    extra["xmux"] = {xmux[k]: v for k, v in value.items()}
                elif key == "extra" and isinstance(value, dict):
                    extra.update(value)
                else:
                    fail("当前不能转换 xhttp-opts 参数：" + key + "；可改用带原生 extra 的分享链接")
            data["UPSTREAM_XHTTP_EXTRA"] = json.dumps(extra, ensure_ascii=False)
    data = {key: clean(value) for key, value in data.items()}
    if data["UPSTREAM_TRANSPORT"] == "tcp":
        data["UPSTREAM_TRANSPORT"] = "raw"
    if data["UPSTREAM_TRANSPORT"] not in ("", "raw", "xhttp"):
        fail("仅支持 TCP/raw 和 XHTTP 落地")
    if data["UPSTREAM_SECURITY"] not in ("", "tls", "reality"):
        fail("仅支持 TLS / REALITY 落地")
    return data

def validate(data):
    host = data["UPSTREAM_ADDRESS"]
    try:
        ipaddress.ip_address(host)
    except ValueError:
        if len(host) > 253 or not re.fullmatch(
                r"[A-Za-z0-9](?:[A-Za-z0-9.-]*[A-Za-z0-9])?", host):
            fail("落地地址格式不正确")
        if any(not label or len(label) > 63 or label.startswith("-") or label.endswith("-")
               for label in host.split(".")):
            fail("落地地址格式不正确")
    if not data["UPSTREAM_PORT"].isdigit() or not 1 <= int(data["UPSTREAM_PORT"]) <= 65535:
        fail("落地端口必须为 1–65535")
    if not re.fullmatch(r"[0-9a-fA-F]{8}(?:-[0-9a-fA-F]{4}){3}-[0-9a-fA-F]{12}",
                        data["UPSTREAM_UUID"]):
        fail("落地 UUID 格式不正确")
    if data["UPSTREAM_TRANSPORT"] not in ("raw", "xhttp"):
        fail("请选择落地传输方式 raw 或 xhttp")
    if data["UPSTREAM_SECURITY"] not in ("tls", "reality"):
        fail("请选择落地安全方式 tls 或 reality")
    flow = data["UPSTREAM_FLOW"]
    if flow not in ("", "xtls-rprx-vision", "xtls-rprx-vision-udp443"):
        fail("不支持该落地 flow")
    if data["UPSTREAM_TRANSPORT"] == "xhttp" and flow:
        fail("XHTTP + encryption=none 不能设置 Vision flow，请核对落地配置")
    boolean(data["UPSTREAM_ALLOW_INSECURE"])
    if data["UPSTREAM_SECURITY"] == "reality":
        if not re.fullmatch(r"[A-Za-z0-9_-]{43}", data["UPSTREAM_REALITY_PUBLIC_KEY"]):
            fail("REALITY 落地需要有效的 public-key/pbk（43 位）")
        if not re.fullmatch(r"(?:[0-9a-fA-F]{2}){0,8}", data["UPSTREAM_REALITY_SHORT_ID"]):
            fail("REALITY short-id/sid 必须是最多 16 位、偶数长度的十六进制")
    if data["UPSTREAM_TRANSPORT"] == "xhttp":
        if not data["UPSTREAM_XHTTP_PATH"].startswith("/"):
            fail("XHTTP path 必须以 / 开头")
        if data["UPSTREAM_XHTTP_MODE"] not in ("auto", "stream-one", "stream-up", "packet-up"):
            fail("XHTTP mode 不正确")
        if not isinstance(json.loads(data["UPSTREAM_XHTTP_EXTRA"]), dict):
            fail("XHTTP extra 必须是 JSON 对象")

def upstream_outbound(data):
    validate(data)
    settings = {"address": data["UPSTREAM_ADDRESS"], "port": int(data["UPSTREAM_PORT"]),
                "id": data["UPSTREAM_UUID"], "encryption": "none"}
    if data["UPSTREAM_FLOW"]:
        settings["flow"] = data["UPSTREAM_FLOW"]
    stream = {"network": data["UPSTREAM_TRANSPORT"], "security": data["UPSTREAM_SECURITY"]}
    if stream["security"] == "tls":
        tls = {"serverName": data["UPSTREAM_SNI"] or data["UPSTREAM_ADDRESS"],
               "allowInsecure": boolean(data["UPSTREAM_ALLOW_INSECURE"]),
               "fingerprint": data["UPSTREAM_FINGERPRINT"] or "chrome"}
        if data["UPSTREAM_ALPN"]:
            tls["alpn"] = data["UPSTREAM_ALPN"].split(",")
        stream["tlsSettings"] = tls
    else:
        reality = {"serverName": data["UPSTREAM_SNI"] or data["UPSTREAM_ADDRESS"],
                   "fingerprint": data["UPSTREAM_FINGERPRINT"] or "chrome",
                   "publicKey": data["UPSTREAM_REALITY_PUBLIC_KEY"],
                   "shortId": data["UPSTREAM_REALITY_SHORT_ID"]}
        if data["UPSTREAM_REALITY_MLDSA65_VERIFY"]:
            reality["mldsa65Verify"] = data["UPSTREAM_REALITY_MLDSA65_VERIFY"]
        stream["realitySettings"] = reality
    if stream["network"] == "xhttp":
        stream["xhttpSettings"] = {"path": data["UPSTREAM_XHTTP_PATH"],
                                 "host": data["UPSTREAM_XHTTP_HOST"],
                                 "mode": data["UPSTREAM_XHTTP_MODE"],
                                 "extra": json.loads(data["UPSTREAM_XHTTP_EXTRA"])}
    return {"tag": "upstream-vless", "protocol": "vless", "settings": settings, "streamSettings": stream}

def access_data(config, data):
    """Recover exported credentials from the running service's config, not a cache."""
    ids, modes = set(), set()
    data.update(TLS_PORT="", REALITY_PORT="")
    for inbound in config.get("inbounds", []):
        if inbound.get("protocol") != "vless":
            continue
        clients = inbound.get("settings", {}).get("clients", [])
        ids.update(c.get("id") for c in clients)
        stream = inbound.get("streamSettings", {})
        security = stream.get("security")
        if security not in ("tls", "reality"):
            fail("链接导出仅支持 TLS / REALITY 入站")
        if security in modes:
            fail("同种安全模式存在多个入站，不能确定要导出的端口")
        modes.add(security)
        data[security.upper() + "_PORT"] = str(inbound["port"])
        if security == "reality":
            reality = stream["realitySettings"]
            data.update(REALITY_PRIVATE_KEY=reality["privateKey"], REALITY_PUBLIC_KEY="",
                        REALITY_SERVER_NAME=(reality.get("serverNames") or [""])[0],
                        REALITY_SHORT_ID=(reality.get("shortIds") or [""])[0])
    if len(ids) != 1 or None in ids or not modes:
        fail("实例必须只有一个 UUID 才能导出或修改接入信息")
    data["UUID"] = ids.pop()
    data["SECURITY_MODE"] = "dual" if len(modes) == 2 else next(iter(modes))
    return data

def client_nodes(data):
    for security, port in (("reality", data["REALITY_PORT"]), ("tls", data["TLS_PORT"])):
        if not port or data["SECURITY_MODE"] not in (security, "dual"):
            continue
        label = (data["INSTANCE_NAME"] or data["DOMAIN"]) + "-" + security
        node = {"name": label, "type": "vless", "server": data["DOMAIN"], "port": int(port),
                "uuid": data["UUID"], "network": "tcp", "tls": True,
                "flow": "xtls-rprx-vision", "skip-cert-verify": False,
                "client-fingerprint": data["REALITY_FINGERPRINT"] or "chrome",
                "servername": data["DOMAIN"]}
        if security == "reality":
            node.update(servername=data["REALITY_SERVER_NAME"], **{
                "reality-opts": {"public-key": data["REALITY_PUBLIC_KEY"],
                                 "short-id": data["REALITY_SHORT_ID"]}})
        yield security, node

def records():
    root = Path(os.environ["MANAGER_DIR"]) / "instances"
    for path in sorted(root.glob("*/state.json")):
        try:
            state = read_json(path)
            if state.get("INSTANCE_ID") != path.parent.name:
                fail("实例编号与目录不匹配")
            yield state
        except (ValueError, OSError):
            print("无法读取实例登记：" + str(path), file=sys.stderr)

def legacy(app):
    config = read_json(Path(app) / "config.json")
    data = environment(STATE + UP)
    data.update(APP_DIR=str(Path(app).resolve()), INSTANCE_IMPORTED="true",
                NODE_MODE="direct", CERT_MODE="existing")
    for filename in ("install.env", "reality.env"):
        path = Path(app) / filename
        if path.exists():
            for line in path.read_text().splitlines():
                key, sep, value = line.partition("=")
                if sep and key in STATE and key not in (
                        "INSTANCE_ID", "INSTANCE_NAME", "INSTANCE_IMPORTED", "APP_DIR",
                        "SERVICE_NAME", "XRAY_BIN", "INIT_SYSTEM"):
                    data[key] = value
    ids = set()
    modes = set()
    for inbound in config.get("inbounds", []):
        if inbound.get("protocol") != "vless":
            fail("旧配置含非 VLESS 入站，暂不能导入")
        clients = inbound.get("settings", {}).get("clients", [])
        ids.update(c.get("id") for c in clients)
        stream = inbound.get("streamSettings", {})
        if stream.get("network", stream.get("method", "raw")) not in ("raw", "tcp"):
            fail("旧配置仅能导入 TCP/raw 入站")
        security = stream.get("security")
        modes.add(security)
        if security == "tls":
            data["TLS_PORT"] = str(inbound["port"])
            cert = stream.get("tlsSettings", {}).get("certificates", [{}])[0]
            data["CERT_FILE"] = cert.get("certificateFile", "")
            data["KEY_FILE"] = cert.get("keyFile", "")
        elif security == "reality":
            data["REALITY_PORT"] = str(inbound["port"])
            reality = stream.get("realitySettings", {})
            data.update(REALITY_PRIVATE_KEY=reality.get("privateKey", ""),
                        REALITY_SHORT_ID=(reality.get("shortIds") or [""])[0],
                        REALITY_SERVER_NAME=(reality.get("serverNames") or [""])[0],
                        REALITY_TARGET=reality.get("target", reality.get("dest", "")))
        else:
            fail("旧配置仅能导入 TLS / REALITY 入站")
    if len(ids) != 1 or None in ids:
        fail("旧配置必须只有一个 UUID；dual 两个入站可以共用同一 UUID")
    data["UUID"] = ids.pop()
    data["SECURITY_MODE"] = "dual" if modes == {"tls", "reality"} else next(iter(modes))
    routes = {rule.get("outboundTag") for rule in config.get("routing", {}).get("rules", [])
              if rule.get("inboundTag")}
    if len(routes) != 1 or not routes <= {"direct", "upstream-vless"}:
        fail("旧配置路由结构不能安全导入")
    if routes == {"upstream-vless"}:
        outbound = next(o for o in config["outbounds"] if o.get("tag") == "upstream-vless")
        settings = outbound["settings"]
        if "vnext" in settings:
            server = settings["vnext"][0]
            settings = dict(server["users"][0], address=server["address"], port=server["port"])
        stream = outbound["streamSettings"]
        reality = stream.get("realitySettings", {})
        tls = stream.get("tlsSettings", {})
        xhttp = stream.get("xhttpSettings", {})
        data.update(NODE_MODE="relay", UPSTREAM_ADDRESS=settings["address"],
                    UPSTREAM_PORT=str(settings["port"]), UPSTREAM_UUID=settings["id"],
                    UPSTREAM_FLOW=settings.get("flow", ""), UPSTREAM_SECURITY=stream["security"],
                    UPSTREAM_TRANSPORT=stream.get("network", stream.get("method", "raw")),
                    UPSTREAM_SNI=reality.get("serverName", tls.get("serverName", "")),
                    UPSTREAM_FINGERPRINT=reality.get("fingerprint", tls.get("fingerprint", "chrome")),
                    UPSTREAM_ALLOW_INSECURE=str(tls.get("allowInsecure", False)).lower(),
                    UPSTREAM_REALITY_PUBLIC_KEY=reality.get("publicKey", reality.get("password", "")),
                    UPSTREAM_REALITY_SHORT_ID=reality.get("shortId", ""),
                    UPSTREAM_REALITY_MLDSA65_VERIFY=reality.get("mldsa65Verify", ""),
                    UPSTREAM_XHTTP_PATH=xhttp.get("path", ""), UPSTREAM_XHTTP_HOST=xhttp.get("host", ""),
                    UPSTREAM_XHTTP_MODE=xhttp.get("mode", "auto"),
                    UPSTREAM_XHTTP_EXTRA=json.dumps(xhttp.get("extra", {})),
                    UPSTREAM_ALPN=",".join(tls.get("alpn", [])))
        validate(data)
    return data

try:
    action = sys.argv[1]
    if action == "parse":
        dump(parse_node(Path(sys.argv[2]).read_text().strip()))
    elif action == "upstream-values":
        values(read_json(sys.argv[2]), UP)
    elif action == "validate":
        validate(environment(UP))
    elif action == "outbound":
        dump(upstream_outbound(environment(UP)))
    elif action == "state-save":
        atomic(sys.argv[2], environment(STATE + UP))
    elif action == "state-values":
        values(read_json(sys.argv[2]), STATE + UP)
    elif action == "access-values":
        values(access_data(read_json(sys.argv[2]), environment(STATE + UP)), STATE + UP)
    elif action == "legacy":
        dump(legacy(sys.argv[2]))
    elif action == "list":
        for data in records():
            destination = "Internet" if data["NODE_MODE"] == "direct" else (
                data["UPSTREAM_ADDRESS"] + ":" + data["UPSTREAM_PORT"])
            ports = "/".join(p for p in (data.get("REALITY_PORT"), data.get("TLS_PORT")) if p)
            print("\t".join(clean(x) for x in (data["INSTANCE_ID"], data["INSTANCE_NAME"],
                  data["NODE_MODE"], ports, destination, data["SERVICE_NAME"])))
    elif action == "ports":
        for data in records():
            for key in ("TLS_PORT", "REALITY_PORT", "INTERNAL_HTTP_PORT", "INTERNAL_HTTPS_PORT"):
                if data.get(key):
                    print(data[key])
    elif action == "unique":
        for data in records():
            if data["UUID"].lower() == os.environ["UUID"].lower():
                fail("该 UUID 已被实例 " + data["INSTANCE_ID"] + " 使用")
            if data["APP_DIR"] == os.environ["APP_DIR"] or data["SERVICE_NAME"] == os.environ["SERVICE_NAME"]:
                fail("该目录或服务已登记为实例 " + data["INSTANCE_ID"])
    elif action == "uuid-unique":
        for data in records():
            if data["INSTANCE_ID"] != os.environ["INSTANCE_ID"] and data["UUID"].lower() == os.environ["UUID"].lower():
                fail("该 UUID 已被实例 " + data["INSTANCE_ID"] + " 使用")
    elif action == "edit-access":
        config = read_json(sys.argv[2])
        access_data(config, environment(STATE + UP))
        for inbound in config["inbounds"]:
            if inbound.get("protocol") != "vless":
                continue
            for client in inbound["settings"]["clients"]:
                client["id"] = os.environ["UUID"]
            security = inbound["streamSettings"]["security"]
            inbound["port"] = int(os.environ[security.upper() + "_PORT"])
            if security == "reality":
                reality = inbound["streamSettings"]["realitySettings"]
                reality["privateKey"] = os.environ["REALITY_PRIVATE_KEY"]
                reality["shortIds"] = [os.environ["REALITY_SHORT_ID"]]
        dump(config)
    elif action == "replace-upstream":
        config = read_json(sys.argv[2])
        config["outbounds"] = [upstream_outbound(environment(UP))] + [
            o for o in config["outbounds"] if o.get("tag") != "upstream-vless"]
        dump(config)
    elif action in ("client", "client-yaml"):
        data = environment(STATE)
        common = {"encryption": "none", "type": "tcp", "flow": "xtls-rprx-vision"}
        if action == "client-yaml":
            print("proxies:")
        for security, node in client_nodes(data):
            if action == "client-yaml":
                # JSON flow objects are valid YAML; strings such as short-id stay quoted.
                print("  - " + json.dumps(node, ensure_ascii=False))
                continue
            query = dict(common, security=security, fp=data["REALITY_FINGERPRINT"] or "chrome")
            if security == "reality":
                query.update(sni=data["REALITY_SERVER_NAME"], pbk=data["REALITY_PUBLIC_KEY"],
                             sid=data["REALITY_SHORT_ID"])
            else:
                query["sni"] = data["DOMAIN"]
            print("vless://" + data["UUID"] + "@" + data["DOMAIN"] + ":" + str(node["port"]) + "?" +
                  urlencode(query, quote_via=quote) + "#" + quote(node["name"]))
    else:
        fail("未知内部操作")
except (ValueError, KeyError, IndexError, StopIteration, OSError, TypeError):
    # Do not include parser source snippets or credential-bearing exception reprs.
    message = sys.exc_info()[1]
    if isinstance(message, ValueError) and not isinstance(message, json.JSONDecodeError):
        print("错误：" + str(message), file=sys.stderr)
    else:
        print("错误：配置格式或登记文件不正确，请检查字段", file=sys.stderr)
    sys.exit(1)
PY
}

normalize_domain() {
  echo "$1" | tr '[:upper:]' '[:lower:]' | sed -E 's#^https?://##; s#/.*$##; s/[[:space:]]//g'
}

check_domain() {
  local d="$1"
  [[ "$d" =~ ^[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?(\.[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?)+$ ]]
}

check_ipv4() {
  local ip="$1" IFS=. a b c d
  read -r a b c d <<<"$ip" || return 1
  [[ -n "${a:-}" && -n "${b:-}" && -n "${c:-}" && -n "${d:-}" ]] || return 1
  for n in "$a" "$b" "$c" "$d"; do
    [[ "$n" =~ ^[0-9]+$ ]] || return 1
    (( n >= 0 && n <= 255 )) || return 1
  done
}

check_host() {
  local h="$1"
  check_domain "$h" || check_ipv4 "$h" || {
    [[ "$h" =~ ^[0-9a-fA-F:]+$ ]] &&
      python3 -c 'import ipaddress, sys; ipaddress.IPv6Address(sys.argv[1])' "$h" 2>/dev/null
  }
}

check_port_number() {
  local p="$1"
  [[ "$p" =~ ^[0-9]{1,5}$ ]] || return 1
  (( 10#$p >= 1 && 10#$p <= 65535 ))
}

port_in_use() {
  local p="$1"
  if ss -lnt 2>/dev/null | awk '{print $4}' | grep -E "(:|\\])${p}$" >/dev/null; then
    return 0
  fi
  [[ -d "$MANAGER_DIR/instances" ]] || return 1
  native_helper ports | grep -Fx "$p" >/dev/null
}

port_owned_by_nginx() {
  local p="$1"
  ss -lntp 2>/dev/null | grep -E "(:|\\])${p}[[:space:]]" | grep 'nginx' >/dev/null
}

normalize_node_mode() {
  local v
  v="$(echo "$1" | tr '[:upper:]' '[:lower:]' | tr -d '[:space:]')"
  case "$v" in
    1|direct|exit|landing) echo direct ;;
    2|relay|chain|upstream) echo relay ;;
    *) return 1 ;;
  esac
}

normalize_security_mode() {
  local v
  v="$(echo "$1" | tr '[:upper:]' '[:lower:]' | tr -d '[:space:]')"
  case "$v" in
    1|tls) echo tls ;;
    2|reality) echo reality ;;
    3|dual|both) echo dual ;;
    *) return 1 ;;
  esac
}

normalize_cert_mode() {
  local v
  v="$(echo "$1" | tr '[:upper:]' '[:lower:]' | tr -d '[:space:]')"
  case "$v" in
    1|http|http01|certbot) echo http ;;
    2|cloudflare|cf|dns|dns01) echo cloudflare ;;
    3|existing|manual|file) echo existing ;;
    4|none|skip) echo none ;;
    *) return 1 ;;
  esac
}

normalize_reality_target_mode() {
  local v
  v="$(echo "$1" | tr '[:upper:]' '[:lower:]' | tr -d '[:space:]')"
  case "$v" in
    1|local|self) echo local ;;
    2|remote|external) echo remote ;;
    *) return 1 ;;
  esac
}

uses_tls() { [[ "$SECURITY_MODE" == tls || "$SECURITY_MODE" == dual ]]; }
uses_reality() { [[ "$SECURITY_MODE" == reality || "$SECURITY_MODE" == dual ]]; }
needs_local_cert() { uses_tls || { uses_reality && [[ "$REALITY_TARGET_MODE" == local ]]; }; }
needs_nginx() { needs_local_cert; }

split_host_port() {
  local value="$1"
  if [[ "$value" =~ ^\[([^]]+)\]:([0-9]+)$ ]]; then
    SPLIT_HOST="${BASH_REMATCH[1]}"
    SPLIT_PORT="${BASH_REMATCH[2]}"
  elif [[ "$value" == *:* ]]; then
    SPLIT_HOST="${value%:*}"
    SPLIT_PORT="${value##*:}"
  else
    SPLIT_HOST="$value"
    SPLIT_PORT=443
  fi
}

query_param() {
  local query="$1" wanted="$2" pair key value oldifs="$IFS"
  IFS='&'
  for pair in $query; do
    key="${pair%%=*}"
    value="${pair#*=}"
    if [[ "$key" == "$wanted" ]]; then
      IFS="$oldifs"
      printf '%s' "$value"
      return 0
    fi
  done
  IFS="$oldifs"
  return 1
}

validate_short_id() {
  local sid="$1"
  [[ "$sid" =~ ^[0-9a-fA-F]{0,16}$ ]] || return 1
  (( ${#sid} % 2 == 0 ))
}

parse_upstream_uri() {
  ensure_work_dir
  printf '%s\n' "$1" > "$WORK_DIR/node.input"
  native_helper parse "$WORK_DIR/node.input" > "$WORK_DIR/upstream.json" || return 1
  load_values upstream-values "$WORK_DIR/upstream.json"
}

validate_upstream() {
  native_helper validate
}

detect_platform() {
  if need_cmd apt-get; then PACKAGE_MANAGER=apt
  elif need_cmd dnf; then PACKAGE_MANAGER=dnf
  elif need_cmd yum; then PACKAGE_MANAGER=yum
  elif need_cmd apk; then PACKAGE_MANAGER=apk
  else die "支持 apt、dnf/yum、apk 系统，未找到包管理器"
  fi
  if [[ -z "$INIT_SYSTEM" ]]; then
    if [[ -d /run/systemd/system ]] && need_cmd systemctl; then INIT_SYSTEM=systemd
    elif need_cmd rc-service && need_cmd rc-update; then INIT_SYSTEM=openrc
    else die "未检测到运行中的 systemd 或 OpenRC；请在实际服务器上运行"
    fi
  fi
  case "$INIT_SYSTEM" in
    systemd) need_cmd systemctl || die "找不到 systemctl" ;;
    openrc)
      if ! need_cmd rc-service || ! need_cmd rc-update; then die "找不到 OpenRC 管理命令"; fi ;;
    *) die "INIT_SYSTEM 必须为 systemd/openrc" ;;
  esac
  if [[ "$PACKAGE_MANAGER" == apk && "$NGINX_CONF_DIR" == /etc/nginx/conf.d ]]; then
    NGINX_CONF_DIR=/etc/nginx/http.d
  fi
}

package_install() {
  case "$PACKAGE_MANAGER" in
    apt) apt-get update; DEBIAN_FRONTEND=noninteractive apt-get install -y "$@" ;;
    dnf|yum) "$PACKAGE_MANAGER" install -y "$@" ;;
    apk) apk add --no-cache "$@" ;;
    *) die "请先检测发行版" ;;
  esac
} 9>&-

service_action() {
  local action="$1" service="${2:-$SERVICE_NAME}"
  if [[ "$INIT_SYSTEM" == systemd ]]; then
    case "$action" in
      active) systemctl is-active --quiet "$service" ;;
      enabled) systemctl is-enabled --quiet "$service" ;;
      *) systemctl "$action" "$service" ;;
    esac
  else
    case "$action" in
      active|status) rc-service "$service" status ;;
      enable) rc-update add "$service" default ;;
      disable) rc-update del "$service" default ;;
      enabled) rc-update show default | awk '{print $1}' | grep -Fx "$service" >/dev/null ;;
      *) rc-service "$service" "$action" ;;
    esac
  fi
} 9>&-

service_logs() {
  local follow="${1:-false}"
  if [[ "$INIT_SYSTEM" == systemd ]]; then
    if [[ "$follow" == true ]]; then
      journalctl -u "$SERVICE_NAME" -n 100 -f --no-pager
    else
      journalctl -u "$SERVICE_NAME" -n 100 --no-pager
    fi
  else
    local log="$APP_DIR/logs/service.log"
    [[ -f "$log" ]] || { yellow "尚无服务日志：$log"; return 0; }
    if [[ "$follow" == true ]]; then tail -n 100 -F "$log"; else tail -n 100 "$log"; fi
  fi
}

repair_dpkg_state() {
  [[ "$PACKAGE_MANAGER" == apt ]] || return 0
  if need_cmd dpkg && dpkg --audit 2>/dev/null | grep -q .; then
    yellow "检测到 dpkg 状态异常，尝试修复..."
    dpkg --configure -a >/dev/null 2>&1 || true
    apt-get -f install -y >/dev/null 2>&1 || true
  fi
}

install_base_tools() {
  detect_platform
  repair_dpkg_state
  local packages=() cmd needs_coreutils=0
  need_cmd curl || packages+=(curl)
  need_cmd openssl || packages+=(openssl)
  need_cmd python3 || packages+=(python3)
  need_cmd unzip || packages+=(unzip)
  # Respect working curl-minimal/coreutils-single alternatives on CentOS.
  for cmd in timeout install mktemp chmod cp mv; do
    need_cmd "$cmd" || needs_coreutils=1
  done
  [[ "$needs_coreutils" != 1 ]] || packages+=(coreutils)
  if ! need_cmd ss; then
    case "$PACKAGE_MANAGER" in
      apt) packages+=(iproute2) ;;
      dnf|yum) packages+=(iproute) ;;
      apk) packages+=(iproute2 iproute2-ss) ;;
    esac
  fi
  if ! need_cmd flock; then
    if [[ "$PACKAGE_MANAGER" == apk ]]; then packages+=(flock)
    else packages+=(util-linux)
    fi
  fi
  if ! python3 -c 'import yaml' >/dev/null 2>&1; then
    case "$PACKAGE_MANAGER" in
      apt) packages+=(python3-yaml) ;;
      dnf|yum) packages+=(python3-pyyaml) ;;
      apk) packages+=(py3-yaml) ;;
    esac
  fi
  if [[ "${#packages[@]}" -gt 0 ]]; then
    blue "安装缺少的基础依赖..."
    package_install ca-certificates "${packages[@]}"
  else
    green "基础依赖已满足"
  fi
}

install_xray_if_needed() {
  if [[ -x "$XRAY_BIN" ]]; then
    "$XRAY_BIN" version >/dev/null 2>&1 || die "现有 Xray 无法运行：$XRAY_BIN，请先安装适用于本机的版本"
    green "检测到 Xray：$($XRAY_BIN version 2>/dev/null | head -n1 || true)"
    return 0
  fi

  blue "下载 XTLS 官方 Xray 二进制..."
  ensure_work_dir
  local arch release asset
  case "$(uname -m)" in
    x86_64|amd64) arch=64 ;;
    aarch64|arm64) arch=arm64-v8a ;;
    armv7l) arch=arm32-v7a ;;
    armv6l) arch=arm32-v6 ;;
    i386|i686) arch=32 ;;
    riscv64) arch=riscv64 ;;
    s390x) arch=s390x ;;
    ppc64le) arch=ppc64le ;;
    *) die "暂不支持该 CPU 架构，请预先安装 Xray" ;;
  esac
  release="${XRAY_VERSION:-latest}"
  if [[ "$release" == latest ]]; then
    curl -fsSL --retry 3 https://api.github.com/repos/XTLS/Xray-core/releases/latest \
      -o "$WORK_DIR/release.json"
    release="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["tag_name"])' "$WORK_DIR/release.json")"
  fi
  [[ "$release" =~ ^v[0-9][0-9.]*$ ]] || die "XRAY_VERSION 格式不正确"
  asset="Xray-linux-${arch}.zip"
  curl -fsSL --retry 3 "https://github.com/XTLS/Xray-core/releases/download/${release}/${asset}" \
    -o "$WORK_DIR/xray.zip"
  unzip -q "$WORK_DIR/xray.zip" xray -d "$WORK_DIR/xray"
  "$WORK_DIR/xray/xray" version >/dev/null || die "该 Xray 二进制无法在本机运行"
  mkdir -p "$(dirname "$XRAY_BIN")"
  install -m 755 "$WORK_DIR/xray/xray" "$XRAY_BIN"
  [[ -x "$XRAY_BIN" ]] || die "Xray 安装失败"
  green "Xray 安装完成"
}

install_nginx_if_needed() {
  needs_nginx || return 0

  if [[ "$CERT_MODE" == http ]] && port_in_use 80 && ! port_owned_by_nginx 80; then
    die "CERT_MODE=http 需要 Nginx 使用公网 80 做 HTTP-01，但 80 当前被其它程序占用。请释放 80，或改用 Cloudflare DNS-01 / 已有证书。"
  fi

  if need_cmd nginx; then
    green "检测到系统已安装 Nginx，直接复用"
  else
    blue "未检测到 Nginx，使用系统包管理器安装..."
    if [[ "$PACKAGE_MANAGER" == apk ]]; then package_install nginx nginx-openrc
    else package_install nginx
    fi
  fi

  service_action enable nginx >/dev/null 2>&1 || true
}

install_certbot_for_mode() {
  [[ "$CERT_MODE" == http || "$CERT_MODE" == cloudflare ]] || return 0

  blue "检查 Certbot..."
  local plugin=""
  if [[ "$CERT_MODE" == cloudflare ]]; then
    if [[ "$PACKAGE_MANAGER" == apk ]]; then plugin=certbot-dns-cloudflare
    else plugin=python3-certbot-dns-cloudflare
    fi
  fi
  if [[ "$PACKAGE_MANAGER" == dnf || "$PACKAGE_MANAGER" == yum ]]; then
    # Certbot and DNS plugins are supplied by EPEL on CentOS/RHEL derivatives.
    if ! rpm -q epel-release >/dev/null 2>&1; then package_install epel-release; fi
  fi
  package_install certbot ${plugin:+"$plugin"}
  need_cmd certbot || die "Certbot 安装失败"
}

ask_domain_and_role() {
  if [[ -z "$DOMAIN" ]]; then
    read -rp "请输入节点域名，例如 edge-01.docker.click: " DOMAIN
  fi
  DOMAIN="$(normalize_domain "$DOMAIN")"
  check_domain "$DOMAIN" || die "域名格式不正确：$DOMAIN"

  if [[ -z "$NODE_MODE" ]]; then
    echo
    blue "请选择本机角色："
    echo "  1) direct  普通/落地 VLESS，直接访问 Internet"
    echo "  2) relay   中转 VLESS，继续转发到下一台 VLESS"
    read -rp "请选择 [1/2，默认1]: " NODE_MODE
    [[ -n "$NODE_MODE" ]] || NODE_MODE=1
  fi
  NODE_MODE="$(normalize_node_mode "$NODE_MODE")" || die "NODE_MODE 只能是 direct 或 relay"

  if [[ -z "$SECURITY_MODE" ]]; then
    echo
    blue "请选择入口模式："
    echo "  1) VLESS + TLS + Vision"
    echo "  2) VLESS + REALITY + Vision"
    echo "  3) dual：REALITY 主入口 + TLS 备用"
    read -rp "请选择 [1/2/3，默认2]: " SECURITY_MODE
    [[ -n "$SECURITY_MODE" ]] || SECURITY_MODE=2
  fi
  SECURITY_MODE="$(normalize_security_mode "$SECURITY_MODE")" || die "SECURITY_MODE 只能是 tls/reality/dual"
}

ask_reality_target() {
  uses_reality || return 0

  if [[ -z "$REALITY_TARGET_MODE" ]]; then REALITY_TARGET_MODE=local; fi
  if [[ "$INTERACTIVE" == 1 && "$REALITY_TARGET_MODE" == local && -z "${REALITY_TARGET_EXPLICIT:-}" ]]; then
    echo
    blue "请选择 REALITY 伪装目标："
    echo "  1) local   本机 Nginx HTTPS 站点（推荐，使用自己的域名）"
    echo "  2) remote  指定外部 TLS 站点"
    local choice
    read -rp "请选择 [1/2，默认1]: " choice
    [[ -n "$choice" ]] && REALITY_TARGET_MODE="$choice"
  fi
  REALITY_TARGET_MODE="$(normalize_reality_target_mode "$REALITY_TARGET_MODE")" || die "REALITY_TARGET_MODE 只能是 local/remote"

  if [[ "$REALITY_TARGET_MODE" == remote ]]; then
    if [[ -z "$REALITY_TARGET" ]]; then
      read -rp "请输入 REALITY target，例如 www.example.com:443: " REALITY_TARGET
    fi
    split_host_port "$REALITY_TARGET"
    check_host "$SPLIT_HOST" || die "REALITY target 主机格式不正确"
    check_port_number "$SPLIT_PORT" || die "REALITY target 端口不正确"
    if [[ "$SPLIT_HOST" == *:* ]]; then
      REALITY_TARGET="[${SPLIT_HOST}]:${SPLIT_PORT}"
    else
      REALITY_TARGET="${SPLIT_HOST}:${SPLIT_PORT}"
    fi
    [[ -n "$REALITY_SERVER_NAME" ]] || REALITY_SERVER_NAME="$SPLIT_HOST"
  else
    [[ -n "$REALITY_SERVER_NAME" ]] || REALITY_SERVER_NAME="$DOMAIN"
  fi
}

ask_cert_mode() {
  if ! needs_local_cert; then
    CERT_MODE=none
    return 0
  fi

  if [[ -z "$CERT_MODE" ]]; then
    echo
    blue "请选择证书方式："
    echo "  1) Certbot HTTP-01（推荐：域名不在 Cloudflare 也能用；要求公网 80 可访问）"
    echo "  2) Cloudflare DNS-01（需要 CF_API_TOKEN；不要求公网 80）"
    echo "  3) 使用已有证书文件"
    read -rp "请选择 [1/2/3，默认1]: " CERT_MODE
    [[ -n "$CERT_MODE" ]] || CERT_MODE=1
  fi
  CERT_MODE="$(normalize_cert_mode "$CERT_MODE")" || die "CERT_MODE 不正确"
  [[ "$CERT_MODE" != none ]] || die "当前入口/本地伪装模式需要 TLS 证书，不能 CERT_MODE=none"

  if [[ "$CERT_MODE" == http || "$CERT_MODE" == cloudflare ]]; then
    if [[ -z "$EMAIL" ]]; then read -rp "请输入 Let's Encrypt 邮箱: " EMAIL; fi
    [[ "$EMAIL" == *@* ]] || die "邮箱格式不正确"
  fi

  if [[ "$CERT_MODE" == cloudflare ]]; then
    if [[ -z "$CF_API_TOKEN" ]]; then
      read -rp "Cloudflare API Token: " CF_API_TOKEN
    fi
    CF_API_TOKEN="$(echo "$CF_API_TOKEN" | tr -d '[:space:]')"
    [[ -n "$CF_API_TOKEN" ]] || die "Cloudflare API Token 不能为空"
  elif [[ "$CERT_MODE" == existing ]]; then
    if [[ -z "$CERT_FILE" ]]; then read -rp "fullchain.pem 路径: " CERT_FILE; fi
    if [[ -z "$KEY_FILE" ]]; then read -rp "privkey.pem 路径: " KEY_FILE; fi
    [[ -f "$CERT_FILE" ]] || die "找不到证书：$CERT_FILE"
    [[ -f "$KEY_FILE" ]] || die "找不到私钥：$KEY_FILE"
  fi
}

choose_port_interactive() {
  local label="$1" current="$2" fallback="$3"
  fallback="$(find_free_internal_port "$fallback")"
  if [[ "$INTERACTIVE" == 1 && -n "$INSTANCE_ID" ]]; then
    local suggested="$current"
    if port_in_use "$suggested"; then suggested="$fallback"; fi
    read -rp "${label} 监听端口 [默认 ${suggested}]: " current
    [[ -n "$current" ]] || current="$suggested"
    check_port_number "$current" || die "端口必须为 1–65535" >&2
  fi
  while port_in_use "$current"; do
    yellow "${label} 端口 ${current} 已被占用。" >&2
    if [[ "$INTERACTIVE" != 1 ]]; then
      die "端口 ${current} 已占用；请显式设置其它端口后重试" >&2
    fi
    read -rp "请输入新的 ${label} 端口 [默认 ${fallback}]: " current
    [[ -n "$current" ]] || current="$fallback"
    check_port_number "$current" || { yellow "端口格式不正确" >&2; current="$fallback"; }
    current=$((10#$current))
    (( current < 65535 )) || die "没有可用端口" >&2
    fallback="$(find_free_internal_port "$((current + 1))")"
  done
  printf '%s' "$((10#$current))"
}

choose_public_ports() {
  # Product default: Certbot HTTP-01 still prefers 443 for the actual proxy service.
  # HTTP-01 itself validates on port 80; certificate usage is not limited to 443.
  if [[ "$SECURITY_MODE" == tls ]]; then
    [[ -n "$TLS_PORT" ]] || TLS_PORT="${LISTEN_PORT:-443}"
    check_port_number "$TLS_PORT" || die "TLS_PORT 不正确"
    TLS_PORT="$(choose_port_interactive TLS "$TLS_PORT" 8443)"
  elif [[ "$SECURITY_MODE" == reality ]]; then
    [[ -n "$REALITY_PORT" ]] || REALITY_PORT="${LISTEN_PORT:-443}"
    check_port_number "$REALITY_PORT" || die "REALITY_PORT 不正确"
    REALITY_PORT="$(choose_port_interactive REALITY "$REALITY_PORT" 8443)"
  else
    [[ -n "$REALITY_PORT" ]] || REALITY_PORT="${LISTEN_PORT:-443}"
    [[ -n "$TLS_PORT" ]] || TLS_PORT=8443
    check_port_number "$REALITY_PORT" || die "REALITY_PORT 不正确"
    check_port_number "$TLS_PORT" || die "TLS_PORT 不正确"
    REALITY_PORT="$(choose_port_interactive REALITY "$REALITY_PORT" 9443)"
    if [[ "$TLS_PORT" == "$REALITY_PORT" ]]; then TLS_PORT=$((REALITY_PORT + 1)); fi
    TLS_PORT="$(choose_port_interactive TLS "$TLS_PORT" 10443)"
    [[ "$TLS_PORT" != "$REALITY_PORT" ]] || die "dual 模式两个入口端口不能相同"
  fi
}

ask_upstream() {
  [[ "$NODE_MODE" == relay ]] || return 0
  echo
  blue "配置下一跳 VLESS："

  ensure_work_dir
  if [[ -n "$UPSTREAM_URI" ]]; then
    parse_upstream_uri "$UPSTREAM_URI" || die "落地配置导入失败"
  elif [[ -z "$UPSTREAM_ADDRESS" ]]; then
    blue "粘贴单条 vless:// 或单个 YAML/JSON 节点；多行节点最后输入 END。"
    local line input=""
    read -rp "落地配置（留空进入手工填写）: " line || die "已取消"
    if [[ -n "$line" ]]; then
      input="$line"
      if [[ "$line" != vless://* && "$line" != *"}" && "$line" != *"]" ]]; then
        while IFS= read -r line && [[ "$line" != END ]]; do
          input+=$'\n'"$line"
        done
      fi
      parse_upstream_uri "$input" || die "落地配置导入失败"
    fi
  fi
  complete_upstream
  validate_upstream || die "落地配置校验失败"
  blue "落地：${UPSTREAM_NAME:-未命名}  ${UPSTREAM_ADDRESS}:${UPSTREAM_PORT}"
  blue "传输：${UPSTREAM_TRANSPORT}  安全：${UPSTREAM_SECURITY}  SNI：${UPSTREAM_SNI}"
  if [[ "$UPSTREAM_TRANSPORT" == xhttp ]]; then
    blue "XHTTP path：${UPSTREAM_XHTTP_PATH}  mode：${UPSTREAM_XHTTP_MODE}"
  fi
  if [[ "$INTERACTIVE" == 1 ]]; then
    local confirm
    read -rp "使用这个落地配置？[Y/n]: " confirm || die "已取消"
    [[ "$confirm" != n && "$confirm" != N ]] || die "已取消"
  fi
}

ask_missing() {
  local key="$1" label="$2" default="${3:-}" answer
  [[ -z "${!key}" ]] || return 0
  [[ "$INTERACTIVE" == 1 ]] || die "缺少字段 ${key}，请设置环境变量或提供完整落地配置"
  read -rp "${label}${default:+ [默认 $default]}: " answer || die "已取消"
  printf -v "$key" '%s' "${answer:-$default}"
}

complete_upstream() {
  ask_missing UPSTREAM_ADDRESS "落地服务器域名/IP"
  ask_missing UPSTREAM_PORT "落地端口"
  ask_missing UPSTREAM_UUID "落地 UUID"
  ask_missing UPSTREAM_TRANSPORT "落地传输方式 raw/xhttp" raw
  [[ "$UPSTREAM_TRANSPORT" != tcp ]] || UPSTREAM_TRANSPORT=raw
  ask_missing UPSTREAM_SECURITY "落地安全方式 tls/reality" tls
  if [[ "$INTERACTIVE" == 1 ]]; then
    ask_missing UPSTREAM_SNI "落地 SNI/servername" "$UPSTREAM_ADDRESS"
  else
    [[ -n "$UPSTREAM_SNI" ]] || UPSTREAM_SNI="$UPSTREAM_ADDRESS"
  fi
  if [[ "$UPSTREAM_SECURITY" == reality ]]; then
    ask_missing UPSTREAM_REALITY_PUBLIC_KEY "REALITY public-key/pbk"
    if [[ -z "$UPSTREAM_REALITY_SHORT_ID" && "$INTERACTIVE" == 1 ]]; then
      read -rp "REALITY short-id/sid（落地允许空值时可留空）: " UPSTREAM_REALITY_SHORT_ID || die "已取消"
    fi
  fi
  if [[ "$UPSTREAM_TRANSPORT" == xhttp ]]; then
    ask_missing UPSTREAM_XHTTP_PATH "XHTTP path" /
  fi
}

warn_dns() {
  local public_ip domain_ip
  public_ip="$(curl -4s --max-time 5 https://api.ipify.org || true)"
  domain_ip="$(getent ahostsv4 "$DOMAIN" 2>/dev/null | awk '{print $1; exit}' || true)"

  if [[ -n "$public_ip" && -n "$domain_ip" && "$public_ip" != "$domain_ip" ]]; then
    yellow "警告：本机公网 IPv4 为 ${public_ip}，但 ${DOMAIN} 当前解析到 ${domain_ip}。"
    if [[ "$CERT_MODE" == http ]]; then
      yellow "HTTP-01 需要域名解析到本机，并且公网 80 能访问本机 Nginx。"
    fi
  fi
}

prepare_dirs() {
  mkdir -p "$APP_DIR" "$APP_DIR/certs" "$WEB_ROOT/.well-known/acme-challenge"
  chmod 700 "$APP_DIR"

  cat > "$WEB_ROOT/index.html" <<HTML
<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width,initial-scale=1">
  <title>${DOMAIN}</title>
  <style>
    body{font-family:system-ui,-apple-system,BlinkMacSystemFont,"Segoe UI",sans-serif;max-width:760px;margin:10vh auto;padding:0 24px;color:#222;line-height:1.6}
    h1{font-size:28px}.muted{color:#666}
  </style>
</head>
<body>
  <h1>${DOMAIN}</h1>
  <p>Service is online.</p>
  <p class="muted">Secure application gateway.</p>
</body>
</html>
HTML
  # The manager uses umask 077; Nginx workers must be able to read this public page.
  chmod 644 "$WEB_ROOT/index.html"
}

find_free_internal_port() {
  local p="$1"
  check_port_number "$p" || die "内部端口必须为 1–65535" >&2
  p=$((10#$p))
  while port_in_use "$p"; do
    (( p < 65535 )) || die "没有可用端口" >&2
    p=$((p + 1))
  done
  printf '%s' "$p"
}

choose_internal_ports() {
  needs_nginx || return 0
  # Include the new public ports before its state record is committed.
  local port
  for port in INTERNAL_HTTP_PORT INTERNAL_HTTPS_PORT; do
    printf -v "$port" '%s' "$(find_free_internal_port "${!port}")"
    while [[ "${!port}" == "$TLS_PORT" || "${!port}" == "$REALITY_PORT" ||
             ( "$port" == INTERNAL_HTTPS_PORT && "${!port}" == "$INTERNAL_HTTP_PORT" ) ]]; do
      (( ${!port} < 65535 )) || die "没有可用内部端口"
      printf -v "$port" '%s' "$(find_free_internal_port "$(( ${!port} + 1 ))")"
    done
  done

  if uses_reality && [[ "$REALITY_TARGET_MODE" == local ]]; then
    REALITY_TARGET="127.0.0.1:${INTERNAL_HTTPS_PORT}"
    REALITY_SERVER_NAME="$DOMAIN"
  fi
}

nginx_conf_path() {
  printf '%s/%s-%s.conf' "$NGINX_CONF_DIR" "$SERVICE_NAME" "$DOMAIN"
}

detect_preexisting_nginx_domain() {
  if grep -RhsE \
    "server_name[[:space:]][^;]*${DOMAIN//./\\.}([[:space:];]|$)" \
    "$NGINX_CONF_DIR" /etc/nginx/sites-enabled 2>/dev/null | grep . >/dev/null; then
    NGINX_DOMAIN_PREEXISTED=1
  else
    NGINX_DOMAIN_PREEXISTED=0
  fi
}

write_nginx_challenge_config() {
  [[ "$CERT_MODE" == http ]] || return 0
  local conf="$NGINX_CONF_DIR/xray-chain-http-$DOMAIN.conf"
  local root="$MANAGER_WEB_ROOT/domains/$DOMAIN"
  mkdir -p "$NGINX_CONF_DIR" "$root/.well-known/acme-challenge"
  chmod 755 "$MANAGER_WEB_ROOT" "$MANAGER_WEB_ROOT/domains" "$root" "$root/.well-known" "$root/.well-known/acme-challenge"
  if [[ "$NGINX_DOMAIN_PREEXISTED" == 1 && ! -f "$conf" ]]; then
    die "域名已有其它 Nginx 站点。请选择 DNS-01 或已有证书"
  fi
  if [[ ! -f "$conf" ]]; then
    INSTALL_HTTP_CREATED=1
    cat > "$conf" <<NGINX
# Managed shared HTTP-01 webroot; retained for certificate renewal.
server {
    listen 80;
    listen [::]:80;
    server_name $DOMAIN;
    root $root;
    location ^~ /.well-known/acme-challenge/ { try_files \$uri =404; }
    location / { return 404; }
}
NGINX
  fi
  nginx -t || die "Nginx HTTP-01 配置检查失败"
  if service_action active nginx >/dev/null 2>&1; then service_action reload nginx
  else service_action start nginx
  fi
}

verify_http_challenge() {
  [[ "$CERT_MODE" == http ]] || return 0
  ensure_work_dir
  local root="$MANAGER_WEB_ROOT/domains/$DOMAIN"
  local expected url target attempt status matched response="$WORK_DIR/http-preflight.response"
  local curl_args=()
  HTTP_PROBE_FILE="$(mktemp "$root/.well-known/acme-challenge/xray-preflight-XXXXXXXX")" ||
    die "无法创建 HTTP-01 测试文件：$root/.well-known/acme-challenge"
  expected="xray-http-01:${HTTP_PROBE_FILE##*/}"
  printf '%s' "$expected" > "$HTTP_PROBE_FILE"
  chmod 644 "$HTTP_PROBE_FILE"
  url="http://$DOMAIN/.well-known/acme-challenge/${HTTP_PROBE_FILE##*/}"
  blue "检查 HTTP-01 验证文件：本机 Nginx 和域名访问..."

  for target in local public; do
    matched=0
    curl_args=()
    if [[ "$target" == local ]]; then curl_args=(--resolve "$DOMAIN:80:127.0.0.1"); fi
    for attempt in 1 2 3; do
      status=""
      : > "$response"
      if status="$(curl --noproxy '*' -sS --connect-timeout 3 --max-time 5 \
        "${curl_args[@]}" -o "$response" -w '%{http_code}' "$url" \
        2>"$WORK_DIR/http-preflight.error")" &&
        [[ "$status" == 200 && "$(<"$response")" == "$expected" ]]; then
        matched=1
        break
      fi
      if [[ "$attempt" != 3 ]]; then sleep 1; fi
    done
    if [[ "$matched" != 1 ]]; then
      if [[ "$target" == local ]]; then
        yellow "本机 HTTP-01 自检失败（HTTP ${status:-000}）：$url"
        yellow "请确认实际运行的 Nginx 加载了 $NGINX_CONF_DIR/xray-chain-http-$DOMAIN.conf，且没有同域名的 80 端口站点冲突。"
        yellow "Certbot webroot 应为：$root；使用 nginx -T 检查生效配置。"
        yellow "请同时检查该目录所有父目录的执行权限及 Nginx error.log 中的 Permission denied。"
      else
        yellow "域名 HTTP-01 自检失败（HTTP ${status:-000}）：$url"
        yellow "本机验证文件已可读取；请检查域名的 A/AAAA 记录、80 端口转发及 CDN/重定向规则是否指向本机验证目录。"
      fi
      if [[ -s "$WORK_DIR/http-preflight.error" ]]; then cat "$WORK_DIR/http-preflight.error" >&2; fi
      die "HTTP-01 验证路径不可用，尚未向证书机构提交申请。也可选择 Cloudflare DNS-01。"
    fi
  done
  rm -f -- "$HTTP_PROBE_FILE"
  HTTP_PROBE_FILE=""
  green "HTTP-01 验证文件自检通过"
}

verify_cf_token() {
  [[ "$CERT_MODE" == cloudflare ]] || return 0
  blue "校验 Cloudflare API Token..."
  local response status body
  response="$(curl -sS --connect-timeout 10 --max-time 20 \
    https://api.cloudflare.com/client/v4/user/tokens/verify \
    -H "Authorization: Bearer ${CF_API_TOKEN}" \
    -H 'Content-Type: application/json' \
    -w $'\nHTTP_STATUS:%{http_code}')" || die "Cloudflare Token 校验请求失败"
  status="$(echo "$response" | awk -F 'HTTP_STATUS:' 'NF>1{print $2}' | tail -n1)"
  body="$(echo "$response" | sed '/HTTP_STATUS:/d')"
  if [[ "$status" != 200 ]] || ! echo "$body" | grep -q '"success"[[:space:]]*:[[:space:]]*true'; then
    die "Cloudflare Token 校验失败"
  fi
  green "Cloudflare Token 有效"
}

obtain_or_copy_cert() {
  needs_local_cert || return 0
  local le_live="/etc/letsencrypt/live/${DOMAIN}"

  if [[ "$CERT_MODE" == http ]]; then
    verify_http_challenge
    blue "使用 Certbot webroot 执行 HTTP-01 证书申请..."
    certbot certonly --webroot -w "$MANAGER_WEB_ROOT/domains/$DOMAIN" \
      -d "$DOMAIN" \
      --email "$EMAIL" \
      --agree-tos \
      --no-eff-email \
      --non-interactive \
      --keep-until-expiring
    CERT_FILE="${le_live}/fullchain.pem"
    KEY_FILE="${le_live}/privkey.pem"
  elif [[ "$CERT_MODE" == cloudflare ]]; then
    blue "使用 Cloudflare DNS-01 申请证书..."
    mkdir -p "$MANAGER_DIR/credentials"
    chmod 700 "$MANAGER_DIR/credentials"
    local credentials="$MANAGER_DIR/credentials/$DOMAIN.ini"
    ensure_work_dir
    if [[ -f "$credentials" ]]; then cp -p "$credentials" "$WORK_DIR/cloudflare.backup"; fi
    INSTALL_CF_PATH="$credentials"
    INSTALL_CF_CHANGED=1
    cat > "$credentials" <<CF
# Managed by xray-vless-native-v3.sh
dns_cloudflare_api_token = ${CF_API_TOKEN}
CF
    chmod 600 "$credentials"
    certbot certonly \
      --dns-cloudflare \
      --dns-cloudflare-credentials "$credentials" \
      --dns-cloudflare-propagation-seconds 60 \
      -d "$DOMAIN" \
      --email "$EMAIL" \
      --agree-tos \
      --no-eff-email \
      --non-interactive \
      --keep-until-expiring
    CERT_FILE="${le_live}/fullchain.pem"
    KEY_FILE="${le_live}/privkey.pem"
  fi

  [[ -f "$CERT_FILE" ]] || die "证书不存在：$CERT_FILE"
  [[ -f "$KEY_FILE" ]] || die "私钥不存在：$KEY_FILE"

  cp -L "$CERT_FILE" "$APP_DIR/certs/fullchain.pem"
  cp -L "$KEY_FILE" "$APP_DIR/certs/privkey.pem"
  chmod 644 "$APP_DIR/certs/fullchain.pem"
  chmod 600 "$APP_DIR/certs/privkey.pem"
  green "证书已准备完成"
}

primary_public_port() {
  if uses_reality; then printf '%s' "$REALITY_PORT"; else printf '%s' "$TLS_PORT"; fi
}

configure_selinux() {
  needs_nginx || return 0
  need_cmd getenforce || return 0
  [[ "$(getenforce)" == Enforcing ]] || return 0
  if ! need_cmd semanage; then
    local policy_package=policycoreutils-python-utils
    if [[ "$PACKAGE_MANAGER" == yum ]] && rpm -q --qf '%{VERSION}' centos-release 2>/dev/null | grep -q '^7'; then
      policy_package=policycoreutils-python
    fi
    package_install policycoreutils "$policy_package"
  fi
  local port
  for port in "$INTERNAL_HTTP_PORT" "$INTERNAL_HTTPS_PORT"; do
    if ! semanage port -l | python3 -c '
import sys
p=int(sys.argv[1]); found=False
for line in sys.stdin:
    fields=line.split()
    if fields[:2] != ["http_port_t","tcp"]: continue
    for part in "".join(fields[2:]).split(","):
        if not part: continue
        nums=[int(v) for v in part.split("-")]
        if nums[0] <= p <= nums[-1]: found=True
sys.exit(0 if found else 1)' "$port"; then
      semanage port -a -t http_port_t -p tcp "$port" ||
        die "SELinux 端口 $port 已被其它类型使用，请调整 INTERNAL_HTTP_PORT/INTERNAL_HTTPS_PORT"
    fi
  done
  local root="$MANAGER_WEB_ROOT/domains/$DOMAIN"
  mkdir -p "$root/.well-known/acme-challenge"
  # Keep SELinux enabled; label only this manager's website and certificate paths.
  semanage fcontext -a -t httpd_sys_content_t "$MANAGER_WEB_ROOT(/.*)?" 2>/dev/null ||
    semanage fcontext -m -t httpd_sys_content_t "$MANAGER_WEB_ROOT(/.*)?"
  semanage fcontext -a -t httpd_config_t "$APP_DIR/certs(/.*)?" 2>/dev/null ||
    semanage fcontext -m -t httpd_config_t "$APP_DIR/certs(/.*)?"
  restorecon -RF "$MANAGER_WEB_ROOT" "$APP_DIR/certs"
}

write_final_nginx_config() {
  needs_nginx || return 0
  local conf
  conf="$(nginx_conf_path)"
  [[ ! -e "$conf" ]] || die "Nginx 配置已存在：$conf"
  INSTALL_NGINX_CREATED=1
  mkdir -p "$NGINX_CONF_DIR"
  cat > "$conf" <<NGINX
# Managed by xray-vless-native-v3.sh; instance $INSTANCE_ID.
server {
    listen 127.0.0.1:$INTERNAL_HTTP_PORT;
    server_name $DOMAIN;
    root $WEB_ROOT;
    index index.html;
    location / { try_files \$uri \$uri/ /index.html; }
}
server {
    listen 127.0.0.1:$INTERNAL_HTTPS_PORT ssl;
    server_name $DOMAIN;
    ssl_certificate $APP_DIR/certs/fullchain.pem;
    ssl_certificate_key $APP_DIR/certs/privkey.pem;
    ssl_protocols TLSv1.2 TLSv1.3;
    root $WEB_ROOT;
    index index.html;
    location / { try_files \$uri \$uri/ /index.html; }
}
NGINX
  nginx -t || die "Nginx 最终配置检查失败"
  if service_action active nginx >/dev/null 2>&1; then service_action reload nginx
  else service_action start nginx
  fi
  green "Nginx 配置完成"
}

derive_reality_public_key() {
  local out
  out="$("$XRAY_BIN" x25519 -i "$REALITY_PRIVATE_KEY")"
  REALITY_PUBLIC_KEY="$(printf '%s\n' "$out" | awk -F': *' '/Password|PublicKey|Public key/{print $2; exit}')"
  [[ -n "$REALITY_PUBLIC_KEY" ]] || die "无法从当前 REALITY 私钥恢复公钥"
}

prepare_reality_keys() {
  uses_reality || return 0
  blue "准备 REALITY X25519 密钥..."
  local out

  if [[ -n "$REALITY_PRIVATE_KEY" && -z "$REALITY_PUBLIC_KEY" ]]; then
    out="$($XRAY_BIN x25519 -i "$REALITY_PRIVATE_KEY")"
    REALITY_PUBLIC_KEY="$(echo "$out" | awk -F': *' '/Password|PublicKey|Public key/{print $2; exit}')"
  elif [[ -z "$REALITY_PRIVATE_KEY" ]]; then
    out="$($XRAY_BIN x25519)"
    REALITY_PRIVATE_KEY="$(echo "$out" | awk -F': *' '/PrivateKey|Private key/{print $2; exit}')"
    REALITY_PUBLIC_KEY="$(echo "$out" | awk -F': *' '/Password|PublicKey|Public key/{print $2; exit}')"
  fi

  [[ -n "$REALITY_PRIVATE_KEY" && -n "$REALITY_PUBLIC_KEY" ]] || die "REALITY 密钥生成失败"
  [[ -n "$REALITY_SHORT_ID" ]] || REALITY_SHORT_ID="$(openssl rand -hex 8)"
  validate_short_id "$REALITY_SHORT_ID" || die "REALITY_SHORT_ID 格式不正确"
}

save_reality_env() {
  uses_reality || return 0
  cat > "$APP_DIR/reality.env" <<ENV
REALITY_PRIVATE_KEY=${REALITY_PRIVATE_KEY}
REALITY_PUBLIC_KEY=${REALITY_PUBLIC_KEY}
REALITY_SHORT_ID=${REALITY_SHORT_ID}
REALITY_TARGET=${REALITY_TARGET}
REALITY_SERVER_NAME=${REALITY_SERVER_NAME}
ENV
  chmod 600 "$APP_DIR/reality.env"
}

generate_reality_keys() {
  prepare_reality_keys
  save_reality_env
}

build_upstream_outbounds() {
  if [[ "$NODE_MODE" == relay ]]; then
    native_helper outbound
    printf ',\n'
  fi
  printf '{"tag":"direct","protocol":"freedom"}\n'
}

write_xray_config() {
  blue "写入 Xray 配置..."
  local inbounds="" inbound_tags="" route_outbound outbounds
  route_outbound=direct
  [[ "$NODE_MODE" == relay ]] && route_outbound='upstream-vless'

  if uses_tls; then
    inbounds=$(cat <<JSON
    {
      "tag": "vless-tls-in",
      "listen": "0.0.0.0",
      "port": ${TLS_PORT},
      "protocol": "vless",
      "settings": {
        "clients": [
          {"id": "${UUID}", "flow": "xtls-rprx-vision"}
        ],
        "decryption": "none",
        "fallbacks": [
          {"dest": "127.0.0.1:${INTERNAL_HTTP_PORT}"}
        ]
      },
      "streamSettings": {
        "method": "raw",
        "security": "tls",
        "tlsSettings": {
          "minVersion": "1.2",
          "certificates": [
            {
              "certificateFile": "${APP_DIR}/certs/fullchain.pem",
              "keyFile": "${APP_DIR}/certs/privkey.pem"
            }
          ]
        }
      }
    }
JSON
)
    inbound_tags='"vless-tls-in"'
  fi

  if uses_reality; then
    [[ -n "$inbounds" ]] && inbounds+=","$'\n'
    inbounds+=$(cat <<JSON
    {
      "tag": "vless-reality-in",
      "listen": "0.0.0.0",
      "port": ${REALITY_PORT},
      "protocol": "vless",
      "settings": {
        "clients": [
          {"id": "${UUID}", "flow": "xtls-rprx-vision"}
        ],
        "decryption": "none"
      },
      "streamSettings": {
        "method": "raw",
        "security": "reality",
        "realitySettings": {
          "show": false,
          "target": "${REALITY_TARGET}",
          "xver": 0,
          "serverNames": ["${REALITY_SERVER_NAME}"],
          "privateKey": "${REALITY_PRIVATE_KEY}",
          "shortIds": ["${REALITY_SHORT_ID}"],
          "limitFallbackUpload": {
            "afterBytes": 0,
            "bytesPerSec": 1048576,
            "burstBytesPerSec": 2097152
          },
          "limitFallbackDownload": {
            "afterBytes": 0,
            "bytesPerSec": 4194304,
            "burstBytesPerSec": 8388608
          }
        }
      }
    }
JSON
)
    if [[ -n "$inbound_tags" ]]; then
      inbound_tags+=', "vless-reality-in"'
    else
      inbound_tags='"vless-reality-in"'
    fi
  fi

  outbounds="$(build_upstream_outbounds)"

  cat > "$APP_DIR/config.json" <<JSON
{
  "log": {
    "loglevel": "info"
  },
  "routing": {
    "domainStrategy": "AsIs",
    "rules": [
      {
        "type": "field",
        "inboundTag": [${inbound_tags}],
        "outboundTag": "${route_outbound}"
      }
    ]
  },
  "inbounds": [
${inbounds}
  ],
  "outbounds": [
${outbounds}
  ]
}
JSON
  chmod 600 "$APP_DIR/config.json"
}

write_systemd_service() {
  INSTALL_SERVICE_CREATED=1
  if [[ "$INIT_SYSTEM" == systemd ]]; then
    mkdir -p "$SYSTEMD_DIR"
    cat > "$SYSTEMD_DIR/$SERVICE_NAME.service" <<SERVICE
[Unit]
Description=Xray VLESS instance $INSTANCE_ID
After=network-online.target nss-lookup.target
Wants=network-online.target

[Service]
Type=simple
User=root
ExecStart=$XRAY_BIN run -config $APP_DIR/config.json
Restart=on-failure
RestartSec=3s
LimitNOFILE=1000000
UMask=0077

[Install]
WantedBy=multi-user.target
SERVICE
    systemctl daemon-reload
  else
    mkdir -p "$OPENRC_DIR" "$APP_DIR/logs"
    touch "$APP_DIR/logs/service.log"
    chmod 600 "$APP_DIR/logs/service.log"
    cat > "$OPENRC_DIR/$SERVICE_NAME" <<SERVICE
#!/sbin/openrc-run
description="Xray VLESS instance $INSTANCE_ID"
supervisor="supervise-daemon"
command="$XRAY_BIN"
command_args="run -config $APP_DIR/config.json"
pidfile="/run/$SERVICE_NAME.pid"
respawn_delay=3
respawn_max=0
output_log="$APP_DIR/logs/service.log"
error_log="$APP_DIR/logs/service.log"
depend() {
    need net
    after firewall
}
SERVICE
    chmod 755 "$OPENRC_DIR/$SERVICE_NAME"
  fi
}

validate_xray_config() {
  blue "校验 Xray 配置..."
  test_xray_config_file "$APP_DIR/config.json"
  green "Xray 配置校验通过"
}

test_xray_config_file() {
  ensure_work_dir
  # Atomic-update candidates have random suffixes; Xray cannot infer their format.
  if "$XRAY_BIN" run -test -format json -config "$1" > "$WORK_DIR/xray-validation.log" 2>&1; then
    return 0
  fi
  cat "$WORK_DIR/xray-validation.log" >&2
  die "Xray 配置校验失败，原配置未替换"
}

check_upstream_reachability() {
  [[ "$NODE_MODE" == relay ]] || return 0
  blue "检查下一跳 TCP $UPSTREAM_ADDRESS:$UPSTREAM_PORT..."
  # Expand positional arguments inside the child shell, never in shell source.
  # shellcheck disable=SC2016
  if timeout 6 bash -c 'exec 3<>"/dev/tcp/$1/$2"' bash "$UPSTREAM_ADDRESS" "$UPSTREAM_PORT" 2>/dev/null; then
    green "下一跳 TCP 端口可达（尚未验证 VLESS 登录及出网）"
  else
    yellow "下一跳当前不可达；配置已保留，请检查落地服务和网络。"
  fi
}

start_xray() {
  service_action enable
  service_action start
  sleep 1
  if ! service_action active >/dev/null 2>&1; then
    service_logs || true
    die "Xray 服务启动失败"
  fi
  green "$SERVICE_NAME 已启动"
}

setup_cert_renew_hook() {
  [[ "$CERT_MODE" == http || "$CERT_MODE" == cloudflare ]] || return 0
  mkdir -p "$RENEW_HOOK_DIR"
  local hook="$RENEW_HOOK_DIR/$SERVICE_NAME.sh"
  [[ ! -e "$hook" ]] || die "续期脚本已存在：$hook"
  INSTALL_HOOK_CREATED=1
  cat > "$hook" <<HOOK
#!/usr/bin/env bash
set -Eeuo pipefail
# Only copy this instance's own certificate; unrelated renewals are ignored.
[[ "\${RENEWED_LINEAGE:-}" == "/etc/letsencrypt/live/$DOMAIN" ]] || exit 0
cp -L "\$RENEWED_LINEAGE/fullchain.pem" "$APP_DIR/certs/fullchain.pem"
cp -L "\$RENEWED_LINEAGE/privkey.pem" "$APP_DIR/certs/privkey.pem"
chmod 644 "$APP_DIR/certs/fullchain.pem"
chmod 600 "$APP_DIR/certs/privkey.pem"
nginx -t
if [[ "$INIT_SYSTEM" == systemd ]]; then
  if systemctl is-active --quiet nginx; then systemctl reload nginx; fi
  if systemctl is-active --quiet "$SERVICE_NAME"; then systemctl restart "$SERVICE_NAME"; fi
else
  if rc-service nginx status >/dev/null 2>&1; then rc-service nginx reload; fi
  if rc-service "$SERVICE_NAME" status >/dev/null 2>&1; then rc-service "$SERVICE_NAME" restart; fi
fi
HOOK
  chmod 700 "$hook"
  if [[ "$INIT_SYSTEM" == systemd ]]; then
    systemctl enable --now certbot.timer >/dev/null 2>&1 ||
      systemctl enable --now certbot-renew.timer >/dev/null 2>&1 ||
      yellow "未找到 Certbot timer，请配置定时运行 certbot renew"
  else
    mkdir -p /etc/periodic/daily
    if [[ ! -e /etc/periodic/daily/xray-chain-certbot ]]; then
      printf '#!/bin/sh\ncertbot renew --quiet\n' > /etc/periodic/daily/xray-chain-certbot
      chmod 755 /etc/periodic/daily/xray-chain-certbot
    fi
    service_action enable crond >/dev/null 2>&1 || true
    service_action active crond >/dev/null 2>&1 || service_action start crond
  fi
}

render_client_info() {
    printf '租户：%s\n角色：%s\n入口域名：%s\nUUID：%s\n' "$INSTANCE_NAME" "$NODE_MODE" "$DOMAIN" "$UUID"
    if [[ "$NODE_MODE" == relay ]]; then
      printf '绑定落地：%s:%s (%s + %s)\n' "$UPSTREAM_ADDRESS" "$UPSTREAM_PORT" "$UPSTREAM_SECURITY" "$UPSTREAM_TRANSPORT"
    fi
    printf '\n分享链接：\n'
    native_helper client || return 1
    printf '\nClash / Mihomo 节点（复制到客户端配置）：\n'
    native_helper client-yaml
}

write_client_info() {
  EXPORT_PATH="$(mktemp "$APP_DIR/.client.XXXXXXXX")"
  render_client_info > "$EXPORT_PATH" || return 1
  chmod 600 "$EXPORT_PATH"
  mv -f "$EXPORT_PATH" "$APP_DIR/client.txt"
  EXPORT_PATH=""
}

sync_access_info() {
  load_values access-values "$APP_DIR/config.json" || die "无法恢复当前接入信息"
  if uses_reality; then derive_reality_public_key; fi
}

show_client_info() {
  sync_access_info
  render_client_info
  printf '\n提示：以后从主菜单“查看租户 VLESS 链接”即可再次查看。\n'
}

save_install_env() {
  cat > "$APP_DIR/install.env" <<ENV
DOMAIN=${DOMAIN}
NODE_MODE=${NODE_MODE}
SECURITY_MODE=${SECURITY_MODE}
CERT_MODE=${CERT_MODE}
TLS_PORT=${TLS_PORT}
REALITY_PORT=${REALITY_PORT}
REALITY_TARGET_MODE=${REALITY_TARGET_MODE}
REALITY_TARGET=${REALITY_TARGET}
REALITY_SERVER_NAME=${REALITY_SERVER_NAME}
INTERNAL_HTTP_PORT=${INTERNAL_HTTP_PORT}
INTERNAL_HTTPS_PORT=${INTERNAL_HTTPS_PORT}
ENV
  chmod 600 "$APP_DIR/install.env"
}

show_result() {
  echo
  green "安装完成：$INSTANCE_NAME ($INSTANCE_ID)"
  blue "配置：$APP_DIR/config.json"
  blue "服务：$SERVICE_NAME ($INIT_SYSTEM)"
  cat "$APP_DIR/client.txt"
  printf '\n请在云安全组 / 防火墙允许入口 TCP 端口：%s %s\n' "$REALITY_PORT" "$TLS_PORT"
  printf '\n再次执行脚本进入管理菜单；也可执行：bash %s manage %s\n' "$SCRIPT_PATH" "$INSTANCE_ID"
}

ensure_work_dir() {
  if [[ -z "$WORK_DIR" ]]; then
    WORK_DIR="$(mktemp -d "${TMPDIR:-/tmp}/xray-chain.XXXXXXXX")"
    chmod 700 "$WORK_DIR"
  fi
}

load_values() {
  local action="$1" path="$2" key value
  ensure_work_dir
  native_helper "$action" "$path" > "$WORK_DIR/values.bin" || return 1
  while IFS= read -r -d '' key && IFS= read -r -d '' value; do
    case " ${STATE_FIELDS[*]} ${UPSTREAM_FIELDS[*]} " in
      *" $key "*) printf -v "$key" '%s' "$value" ;;
      *) die "未知配置字段" ;;
    esac
  done < "$WORK_DIR/values.bin"
  rm -f "$WORK_DIR/values.bin"
}

validate_instance_id() {
  [[ "$1" =~ ^[a-zA-Z0-9][a-zA-Z0-9_-]{0,39}$ ]] ||
    die "租户编号使用 1–40 位英文字母、数字、下划线、短横线"
}

safe_path() {
  [[ "$1" == /* && "$1" =~ ^[a-zA-Z0-9_./-]+$ && "$1" != *"/../"* && "$1" != */.. ]] ||
    die "路径必须为不含空格、特殊字符或 .. 的绝对路径：$1"
}

manager_lock() {
  need_cmd flock || die "缺少 flock，请先安装 util-linux"
  safe_path "$MANAGER_DIR"
  mkdir -p "$MANAGER_DIR/instances"
  chmod 700 "$MANAGER_DIR" "$MANAGER_DIR/instances"
  exec 9>"$MANAGER_DIR/.lock"
  flock -n 9 || die "另一个管理操作正在执行，请稍后重试"
}

record_path() {
  printf '%s/instances/%s/state.json' "$MANAGER_DIR" "$INSTANCE_ID"
}

load_instance() {
  validate_instance_id "$1"
  local path="$MANAGER_DIR/instances/$1/state.json" requested="$1"
  [[ -f "$path" ]] || die "实例不存在：$1"
  load_values state-values "$path" || die "无法读取实例登记"
  [[ "$INSTANCE_ID" == "$requested" ]] || die "实例编号与登记不一致"
  safe_path "$APP_DIR"
  safe_path "$WEB_ROOT"
  safe_path "$XRAY_BIN"
  check_domain "$DOMAIN" || die "实例登记的域名不正确"
  [[ "$SERVICE_NAME" =~ ^[a-zA-Z0-9][a-zA-Z0-9_-]{0,79}$ ]] || die "服务名不正确"
  [[ -f "$APP_DIR/config.json" ]] || die "找不到实例配置：$APP_DIR/config.json"
}

restore_file() {
  local source="$1" destination="$2" temp
  temp="$(mktemp "$(dirname "$destination")/.restore.XXXXXXXX")" || return 1
  if cp -p "$source" "$temp" && mv -f "$temp" "$destination"; then return 0; fi
  rm -f "$temp"
  return 1
}

cleanup() {
  local result=$?
  trap - EXIT INT TERM
  set +e
  [[ -z "$HTTP_PROBE_FILE" ]] || rm -f -- "$HTTP_PROBE_FILE"
  if [[ "$UPDATE_PENDING" == 1 ]]; then
    yellow "操作未完成，恢复原配置和租户信息..." >&2
    restore_file "$WORK_DIR/config.backup" "$APP_DIR/config.json" ||
      red "恢复失败：$APP_DIR/config.json" >&2
    restore_file "$WORK_DIR/state.backup" "$(record_path)" ||
      red "恢复失败：$(record_path)" >&2
    local filename
    for filename in client.txt reality.env install.env; do
      if [[ -f "$WORK_DIR/$filename.backup" ]]; then
        restore_file "$WORK_DIR/$filename.backup" "$APP_DIR/$filename" ||
          red "恢复失败：$APP_DIR/$filename" >&2
      else
        rm -f "$APP_DIR/$filename"
      fi
    done
    if [[ "$UPDATE_WAS_ACTIVE" == 1 ]]; then
      service_action restart || red "原服务恢复启动失败：$SERVICE_NAME" >&2
    fi
  fi
  if [[ "$DELETE_PENDING" == 1 ]]; then
    yellow "删除未完成，恢复原服务和站点..." >&2
    local entry
    for entry in service nginx hook; do
      if [[ -f "$WORK_DIR/delete.$entry.backup" ]]; then
        local destination
        destination="$(cat "$WORK_DIR/delete.$entry.path")"
        restore_file "$WORK_DIR/delete.$entry.backup" "$destination" ||
          red "恢复失败：$destination" >&2
      fi
    done
    if [[ "$INIT_SYSTEM" == systemd ]]; then systemctl daemon-reload; fi
    if [[ -f "$WORK_DIR/delete.nginx.backup" ]]; then nginx -t && service_action reload nginx; fi
    if [[ "$DELETE_WAS_ENABLED" == 1 ]]; then service_action enable; else service_action disable; fi
    if [[ "$DELETE_WAS_ACTIVE" == 1 ]]; then service_action start; fi
  fi
  if [[ "$INSTALL_PENDING" == 1 ]]; then
    yellow "安装未完成，清理本次新增实例..." >&2
    if [[ "$INSTALL_SERVICE_CREATED" == 1 ]]; then
      service_action stop >/dev/null 2>&1
      service_action disable >/dev/null 2>&1
      if [[ "$INIT_SYSTEM" == systemd ]]; then
        rm -f "$SYSTEMD_DIR/$SERVICE_NAME.service"
        systemctl daemon-reload
      else
        rm -f "$OPENRC_DIR/$SERVICE_NAME"
      fi
    fi
    [[ "$INSTALL_HOOK_CREATED" != 1 ]] || rm -f "$RENEW_HOOK_DIR/$SERVICE_NAME.sh"
    if [[ "$INSTALL_HTTP_CREATED" == 1 ]]; then
      rm -f "$NGINX_CONF_DIR/xray-chain-http-$DOMAIN.conf"
    fi
    if [[ "$INSTALL_NGINX_CREATED" == 1 ]]; then
      rm -f "$(nginx_conf_path)"
    fi
    if [[ "$INSTALL_HTTP_CREATED" == 1 || "$INSTALL_NGINX_CREATED" == 1 ]]; then
      nginx -t && service_action reload nginx
    fi
    if [[ "$INSTALL_CF_CHANGED" == 1 ]]; then
      if [[ -f "$WORK_DIR/cloudflare.backup" ]]; then
        restore_file "$WORK_DIR/cloudflare.backup" "$INSTALL_CF_PATH" ||
          red "凭据恢复失败：$INSTALL_CF_PATH" >&2
      else
        rm -f "$INSTALL_CF_PATH"
      fi
    fi
    if [[ "$INSTALL_APP_CREATED" == 1 && "$APP_DIR" == "$MANAGER_DIR/instances/$INSTANCE_ID" ]]; then
      rm -rf -- "$APP_DIR"
      rm -rf -- "$WEB_ROOT"
    fi
  fi
  [[ -z "${CANDIDATE_PATH:-}" ]] || rm -f "$CANDIDATE_PATH"
  [[ -z "$EXPORT_PATH" ]] || rm -f "$EXPORT_PATH"
  if [[ -n "$WORK_DIR" && -d "$WORK_DIR" ]]; then
    rm -rf -- "$WORK_DIR" || red "临时目录清理失败：$WORK_DIR" >&2
  fi
  return "$result"
}

import_instance() {
  INSTANCE_ID="${1:-}"
  if [[ -z "$INSTANCE_ID" ]]; then
    read -rp "导入后使用的租户编号: " INSTANCE_ID || die "已取消"
  fi
  validate_instance_id "$INSTANCE_ID"
  local source="${2:-/etc/xray-chain}" service="${3:-xray-chain}"
  safe_path "$source"
  [[ "$service" =~ ^[a-zA-Z0-9][a-zA-Z0-9_-]{0,79}$ ]] || die "旧服务名格式不正确"
  need_cmd python3 || install_base_tools
  manager_lock
  [[ ! -e "$MANAGER_DIR/instances/$INSTANCE_ID" ]] || die "该租户编号已存在"
  SERVICE_NAME="$service"
  [[ -f "$source/config.json" ]] || die "旧目录没有 config.json"
  INSTANCE_NAME="${INSTANCE_NAME:-$INSTANCE_ID}"
  ensure_work_dir
  native_helper legacy "$source" > "$WORK_DIR/import.json" || die "旧配置导入失败"
  load_values state-values "$WORK_DIR/import.json"
  ask_missing DOMAIN "旧节点对外域名"
  DOMAIN="$(normalize_domain "$DOMAIN")"
  check_domain "$DOMAIN" || die "旧节点对外域名格式不正确"
  if [[ "$INIT_SYSTEM" == systemd ]]; then
    systemctl cat "$SERVICE_NAME" | grep -F "$APP_DIR/config.json" >/dev/null ||
      die "旧服务没有使用这个配置文件，请核对服务名和旧目录"
  else
    if [[ ! -f "$OPENRC_DIR/$SERVICE_NAME" ]] ||
      ! grep -F "$APP_DIR/config.json" "$OPENRC_DIR/$SERVICE_NAME" >/dev/null; then
      die "旧 OpenRC 服务没有使用这个配置文件"
    fi
  fi
  native_helper unique
  blue "导入：$INSTANCE_ID  服务：$SERVICE_NAME  目录：$APP_DIR"
  blue "UUID、端口、原配置保持现状；角色：$NODE_MODE"
  if [[ "$INTERACTIVE" == 1 ]]; then
    local confirm
    read -rp "登记这个旧部署？[Y/n]: " confirm || die "已取消"
    [[ "$confirm" != n && "$confirm" != N ]] || die "已取消"
  fi
  mkdir -p "$MANAGER_DIR/instances/$INSTANCE_ID"
  chmod 700 "$MANAGER_DIR/instances/$INSTANCE_ID"
  native_helper state-save "$(record_path)"
  green "已导入，可以进入该实例的运维菜单"
}

list_instances() {
  if [[ ! -d "$MANAGER_DIR/instances" ]]; then yellow "尚未创建或导入实例"; return 0; fi
  local id name mode ports upstream service status
  printf '%-18s %-16s %-8s %-12s %-10s %s\n' "编号" "名称" "角色" "端口" "运行状态" "落地"
  while IFS=$'\t' read -r id name mode ports upstream service; do
    status=stopped
    if service_action active "$service" >/dev/null 2>&1; then status=running; fi
    printf '%-18s %-16s %-8s %-12s %-10s %s\n' "$id" "$name" "$mode" "$ports" "$status" "$upstream"
  done < <(native_helper list)
}

select_instance() {
  [[ -d "$MANAGER_DIR/instances" ]] || { yellow "尚无实例"; return 1; }
  local choice id name mode ports upstream service
  local ids=()
  while IFS=$'\t' read -r id name mode ports upstream service; do
    ids+=("$id")
    printf '  %d) %s (%s)  %s  %s -> %s\n' "${#ids[@]}" "$name" "$id" "$mode" "$ports" "$upstream"
  done < <(native_helper list)
  [[ "${#ids[@]}" -gt 0 ]] || { yellow "尚无实例"; return 1; }
  read -rp "选择实例编号 [0 返回]: " choice || return 1
  [[ "$choice" =~ ^[0-9]+$ && "${#choice}" -le 3 ]] || return 1
  (( 10#$choice >= 1 && 10#$choice <= ${#ids[@]} )) || return 1
  SELECTED_INSTANCE="${ids[$((10#$choice - 1))]}"
}

show_instance() {
  printf '租户：%s (%s)\n角色：%s\n域名：%s\n端口：%s %s\n服务：%s\n' \
    "$INSTANCE_NAME" "$INSTANCE_ID" "$NODE_MODE" "$DOMAIN" "$REALITY_PORT" "$TLS_PORT" "$SERVICE_NAME"
  if [[ "$NODE_MODE" == relay ]]; then
    printf '落地：%s:%s (%s + %s)\n' "$UPSTREAM_ADDRESS" "$UPSTREAM_PORT" "$UPSTREAM_SECURITY" "$UPSTREAM_TRANSPORT"
  fi
  if service_action enabled >/dev/null 2>&1; then echo "开机自启：开启"; else echo "开机自启：关闭"; fi
  service_action status || true
}

begin_instance_update() {
  ensure_work_dir
  cp -p "$APP_DIR/config.json" "$WORK_DIR/config.backup"
  cp -p "$(record_path)" "$WORK_DIR/state.backup"
  local filename
  for filename in client.txt reality.env install.env; do
    if [[ -f "$APP_DIR/$filename" ]]; then cp -p "$APP_DIR/$filename" "$WORK_DIR/$filename.backup"; fi
  done
  UPDATE_WAS_ACTIVE=0
  if service_action active >/dev/null 2>&1; then UPDATE_WAS_ACTIVE=1; fi
  UPDATE_PENDING=1
}

commit_instance_update() {
  local restart="${1:-false}"
  if [[ -n "${CANDIDATE_PATH:-}" ]]; then
    mv -f "$CANDIDATE_PATH" "$APP_DIR/config.json"
    CANDIDATE_PATH=""
  fi
  native_helper state-save "$(record_path)"
  write_client_info
  if [[ "$INSTANCE_IMPORTED" != true ]]; then save_install_env; save_reality_env; fi
  if [[ "$restart" == true && "$UPDATE_WAS_ACTIVE" == 1 ]]; then
    service_action restart
    sleep 1
    service_action active >/dev/null 2>&1 || die "修改后的服务启动失败"
  fi
  UPDATE_PENDING=0
}

make_access_candidate() {
  CANDIDATE_PATH="$(mktemp "$APP_DIR/.config.XXXXXXXX")"
  native_helper edit-access "$APP_DIR/config.json" > "$CANDIDATE_PATH"
  chmod 600 "$CANDIDATE_PATH"
  test_xray_config_file "$CANDIDATE_PATH"
}

confirm_instance_action() {
  local answer
  yellow "$1"
  printf '租户：%s (%s)；入口：%s；服务：%s\n' "$INSTANCE_NAME" "$INSTANCE_ID" "$DOMAIN" "$SERVICE_NAME"
  read -rp "输入租户编号 $INSTANCE_ID 确认，留空取消: " answer || return 1
  [[ "$answer" == "$INSTANCE_ID" ]] || { yellow "已取消"; return 1; }
}

rename_instance() {
  manager_lock
  load_instance "$1"
  sync_access_info
  local name
  read -rp "新的租户显示名称 [当前 $INSTANCE_NAME；留空保持]: " name || return 0
  [[ -n "$name" ]] || return 0
  [[ ! "$name" =~ [[:cntrl:]] ]] || die "名称不能包含控制字符"
  begin_instance_update
  INSTANCE_NAME="$name"
  commit_instance_update false
  green "租户名称已更新"
  render_client_info
}

edit_instance_ports() {
  manager_lock
  load_instance "$1"
  sync_access_info
  local key value old changed=0
  for key in REALITY_PORT TLS_PORT; do
    old="${!key}"
    [[ -n "$old" ]] || continue
    read -rp "${key%_PORT} 入口端口 [当前 $old；留空保持，0 取消]: " value || return 0
    [[ "$value" != 0 ]] || { yellow "已取消"; return 0; }
    [[ -n "$value" ]] || value="$old"
    check_port_number "$value" || die "端口必须为 1–65535"
    value=$((10#$value))
    if [[ "$value" != "$old" ]]; then
      port_in_use "$value" && die "端口 $value 已被占用或被其它实例预留"
      changed=1
    fi
    printf -v "$key" '%s' "$value"
  done
  [[ "$changed" == 1 ]] || { green "端口保持原值"; return 0; }
  [[ -z "$TLS_PORT" || -z "$REALITY_PORT" || "$TLS_PORT" != "$REALITY_PORT" ]] || die "两个入口不能使用同一端口"
  confirm_instance_action "修改端口会使原链接失效；运行中的实例将重启，停止的实例保持停止。" || return 0
  make_access_candidate
  begin_instance_update
  commit_instance_update true
  green "入口端口已更新，请在云安全组允许 TCP：$REALITY_PORT $TLS_PORT"
  render_client_info
}

reset_instance_credentials() {
  manager_lock
  load_instance "$1"
  sync_access_info
  local kind="${2:-uuid}" value
  case "$kind" in uuid|custom-uuid|reality|all) ;; *) die "请选择 UUID 或 REALITY 凭据重置" ;; esac
  if [[ "$kind" == reality || "$kind" == all ]]; then
    uses_reality || die "该实例没有 REALITY 入口"
  fi
  if [[ "$kind" == custom-uuid ]]; then
    read -rp "请输入新的 UUID [留空取消]: " value || return 0
    [[ -n "$value" ]] || return 0
    [[ "$value" =~ ^[0-9a-fA-F]{8}(-[0-9a-fA-F]{4}){3}-[0-9a-fA-F]{12}$ ]] || die "UUID 格式不正确"
  fi
  confirm_instance_action "重置接入凭据会使对应旧链接失效；请重新导入生成的新链接。" || return 0
  if [[ "$kind" == uuid || "$kind" == all ]]; then UUID="$("$XRAY_BIN" uuid)"
  elif [[ "$kind" == custom-uuid ]]; then UUID="$value"
  fi
  native_helper uuid-unique
  if [[ "$kind" == reality || "$kind" == all ]]; then
    REALITY_PRIVATE_KEY=""; REALITY_PUBLIC_KEY=""; REALITY_SHORT_ID=""
    prepare_reality_keys
  fi
  make_access_candidate
  begin_instance_update
  commit_instance_update true
  green "接入凭据已更新"
  render_client_info
}

refresh_client_info() {
  manager_lock
  load_instance "$1"
  sync_access_info
  begin_instance_update
  commit_instance_update false
  green "已从当前服务配置恢复并重新导出接入信息"
  render_client_info
}

delete_instance() {
  manager_lock
  load_instance "$1"
  if [[ "$INSTANCE_IMPORTED" == true ]]; then
    confirm_instance_action "这是导入的旧部署：只取消管理登记，保留原服务、配置、网站和证书。" || return 0
    [[ ! -L "$MANAGER_DIR/instances/$INSTANCE_ID" ]] || die "登记目录不能是符号链接"
    rm -f -- "$(record_path)"
    rmdir "$MANAGER_DIR/instances/$INSTANCE_ID" || yellow "登记已取消；登记目录还有其它文件，已保留"
    green "已取消登记：$INSTANCE_ID"
    return 0
  fi
  [[ "$APP_DIR" == "$MANAGER_DIR/instances/$INSTANCE_ID" &&
     "$WEB_ROOT" == "$MANAGER_WEB_ROOT/instances/$INSTANCE_ID" &&
     "$SERVICE_NAME" == "xray-chain-$INSTANCE_ID" ]] || die "目录或服务归属不符合独立实例规则，停止删除"
  local path conf unit hook entry
  for path in "$MANAGER_DIR" "$MANAGER_DIR/instances" "$APP_DIR" "$MANAGER_WEB_ROOT" "$MANAGER_WEB_ROOT/instances" "$WEB_ROOT"; do
    [[ ! -L "$path" ]] || die "删除路径不能是符号链接：$path"
  done
  conf="$(nginx_conf_path)"; hook="$RENEW_HOOK_DIR/$SERVICE_NAME.sh"
  if [[ "$INIT_SYSTEM" == systemd ]]; then unit="$SYSTEMD_DIR/$SERVICE_NAME.service"
  else unit="$OPENRC_DIR/$SERVICE_NAME"
  fi
  if [[ -f "$conf" ]]; then nginx -t || die "请先修复现有 Nginx 配置，再删除租户"; fi
  confirm_instance_action "将停止并删除这个实例的服务、配置和伪装网页；共享证书及 HTTP 验证站保留。" || return 0
  ensure_work_dir
  for entry in service nginx hook; do
    case "$entry" in service) path="$unit" ;; nginx) path="$conf" ;; hook) path="$hook" ;; esac
    [[ ! -L "$path" ]] || die "实例文件不能是符号链接：$path"
    if [[ -f "$path" ]]; then
      cp -p "$path" "$WORK_DIR/delete.$entry.backup"
      printf '%s' "$path" > "$WORK_DIR/delete.$entry.path"
    fi
  done
  if service_action active >/dev/null 2>&1; then DELETE_WAS_ACTIVE=1; fi
  if service_action enabled >/dev/null 2>&1; then DELETE_WAS_ENABLED=1; fi
  DELETE_PENDING=1
  if [[ "$DELETE_WAS_ACTIVE" == 1 ]]; then service_action stop; fi
  service_action disable
  rm -f -- "$unit" "$conf" "$hook"
  if [[ "$INIT_SYSTEM" == systemd ]]; then systemctl daemon-reload; fi
  if [[ -f "$WORK_DIR/delete.nginx.backup" ]]; then nginx -t; service_action reload nginx; fi
  DELETE_PENDING=0
  rm -rf -- "$WEB_ROOT" "$APP_DIR" || die "实例已停用，但目录未完全删除：$WEB_ROOT；$APP_DIR"
  green "已删除租户：$INSTANCE_ID"
}

repair_instance_site() {
  manager_lock
  load_instance "$1"
  needs_nginx || { green "该实例使用外部伪装目标，没有本机站点"; return 0; }
  [[ "$INSTANCE_IMPORTED" != true && "$WEB_ROOT" == "$MANAGER_WEB_ROOT/instances/$INSTANCE_ID" ]] ||
    die "只能自动修复本脚本创建的独立伪装站；导入站点请检查原目录权限"
  local path
  for path in "$MANAGER_WEB_ROOT" "$MANAGER_WEB_ROOT/instances" "$WEB_ROOT" "$WEB_ROOT/index.html"; do
    [[ ! -L "$path" ]] || die "站点路径不能是符号链接：$path"
  done
  [[ -f "$WEB_ROOT/index.html" ]] || die "首页文件不存在：$WEB_ROOT/index.html"
  chmod 755 "$MANAGER_WEB_ROOT" "$MANAGER_WEB_ROOT/instances" "$WEB_ROOT"
  chmod 644 "$WEB_ROOT/index.html"
  green "伪装站目录和公开首页权限已修复"
}

diagnose_instance() {
  load_instance "$1"
  show_instance
  blue "检查 Xray 配置..."
  if "$XRAY_BIN" run -test -config "$APP_DIR/config.json" >/dev/null 2>&1; then green "Xray 配置有效"
  else yellow "Xray 配置无效，请查看最近日志"
  fi
  local port
  for port in "$REALITY_PORT" "$TLS_PORT"; do
    [[ -n "$port" ]] || continue
    if ss -lnt 2>/dev/null | awk '{print $4}' | grep -E "(:|\\])${port}$" >/dev/null; then green "入口 TCP $port 正在监听"
    else yellow "入口 TCP $port 未监听（若实例已停止，这是预期状态）"
    fi
  done
  if needs_nginx; then
    local status
    ensure_work_dir
    if [[ -f "$WEB_ROOT/index.html" ]]; then
      printf '首页权限：%s\n' "$(stat -c '%a' "$WEB_ROOT/index.html")"
    else yellow "首页文件缺失：$WEB_ROOT/index.html"
    fi
    status="$(curl --noproxy '*' -sS --max-time 6 --resolve "$DOMAIN:$INTERNAL_HTTPS_PORT:127.0.0.1" \
      "https://$DOMAIN:$INTERNAL_HTTPS_PORT/" -o "$WORK_DIR/site.response" -w '%{http_code}' 2>/dev/null || true)"
    if [[ "$status" == 200 ]]; then green "本机 HTTPS 伪装站正常（200）"
    else yellow "本机 HTTPS 伪装站返回 ${status:-连接失败}；403 可在菜单选择修复伪装站权限"
    fi
    if [[ -f "$APP_DIR/certs/fullchain.pem" ]]; then
      if openssl x509 -checkend 604800 -noout -in "$APP_DIR/certs/fullchain.pem" >/dev/null 2>&1; then green "证书有效期超过 7 天"
      else yellow "证书将在 7 天内过期或无法读取，请检查 Certbot 续期任务"
      fi
    fi
  fi
  check_upstream_reachability
  printf '公网访问还需要云安全组允许 TCP：%s %s；本机检测不能确认云安全组规则。\n' "$REALITY_PORT" "$TLS_PORT"
}

edit_instance_menu() {
  local id="$1" choice
  while true; do
    load_instance "$id"
    blue "修改租户：$INSTANCE_NAME ($INSTANCE_ID)"
    echo "  1) 修改显示名称"
    echo "  2) 修改入口监听端口"
    echo "  3) 设置指定 UUID（旧链接失效）"
    [[ "$NODE_MODE" != relay ]] || echo "  4) 更换绑定落地"
    echo "  0) 返回"
    read -rp "请选择: " choice || return 0
    case "$choice" in
      1) run_menu_command rename "$id" ;;
      2) run_menu_command ports "$id" ;;
      3) run_menu_command reset "$id" custom-uuid ;;
      4) if [[ "$NODE_MODE" == relay ]]; then run_menu_command upstream "$id"; else yellow "落地实例无需绑定下一跳"; fi ;;
      0) return 0 ;;
      *) yellow "请选择菜单中的编号" ;;
    esac
  done
}

credentials_menu() {
  local id="$1" choice
  while true; do
    load_instance "$id"
    blue "接入信息：$INSTANCE_NAME ($INSTANCE_ID)"
    echo "  1) 恢复并重新导出链接（保留当前凭据）"
    echo "  2) 重新生成 UUID（旧链接失效）"
    if uses_reality; then
      echo "  3) 重新生成 REALITY 密钥和 short-id（旧 REALITY 链接失效）"
      echo "  4) 重新生成 UUID 和 REALITY 全部凭据（旧链接失效）"
    fi
    echo "  0) 返回"
    read -rp "请选择: " choice || return 0
    case "$choice" in
      1) run_menu_command refresh-client "$id" ;;
      2) run_menu_command reset "$id" uuid ;;
      3|4)
        if uses_reality; then
          if [[ "$choice" == 3 ]]; then run_menu_command reset "$id" reality; else run_menu_command reset "$id" all; fi
        else yellow "该实例没有 REALITY 入口"
        fi ;;
      0) return 0 ;;
      *) yellow "请选择菜单中的编号" ;;
    esac
  done
}

replace_upstream() {
  manager_lock
  load_instance "$1"
  [[ "$NODE_MODE" == relay ]] || die "只有入口实例可以更换落地"
  local key
  for key in "${UPSTREAM_FIELDS[@]}"; do printf -v "$key" '%s' ""; done
  UPSTREAM_FINGERPRINT=chrome
  UPSTREAM_ALLOW_INSECURE=false
  UPSTREAM_XHTTP_MODE=auto
  UPSTREAM_XHTTP_EXTRA='{}'
  ask_upstream
  CANDIDATE_PATH="$(mktemp "$APP_DIR/.config.XXXXXXXX")"
  native_helper replace-upstream "$APP_DIR/config.json" > "$CANDIDATE_PATH"
  chmod 600 "$CANDIDATE_PATH"
  test_xray_config_file "$CANDIDATE_PATH"
  sync_access_info
  begin_instance_update
  commit_instance_update true
  green "落地绑定已更新，入口 UUID 和端口保持不变"
}

run_menu_command() {
  # Run each operation in a fresh shell so conditional calls cannot disable errexit.
  local result
  trap ':' INT
  set +e
  bash "$SCRIPT_PATH" "$@"
  result=$?
  set -e
  trap 'exit 130' INT
  if [[ "$result" != 0 && "$result" != 130 ]]; then
    yellow "操作未完成（退出码 $result），已返回菜单"
  fi
  return 0
}

instance_menu() {
  local id="$1" choice
  while true; do
    [[ -f "$MANAGER_DIR/instances/$id/state.json" ]] || return 0
    load_instance "$id"
    echo
    blue "租户：$INSTANCE_NAME ($INSTANCE_ID)  $NODE_MODE"
    echo "  1) 查看状态和绑定落地"
    echo "  2) 查看最近日志"
    echo "  3) 持续查看日志（Ctrl+C 返回）"
    echo "  4) 启动"
    echo "  5) 停止"
    echo "  6) 重启"
    echo "  7) 开启开机自启"
    echo "  8) 关闭开机自启"
    echo "  9) 查看 VLESS 链接和 Clash / Mihomo 节点"
    [[ "$NODE_MODE" != relay ]] || echo " 10) 更换绑定落地"
    echo " 11) 修改租户信息（名称 / 端口 / UUID）"
    echo " 12) 恢复链接 / 重新生成接入凭据"
    echo " 13) 删除租户 / 取消旧部署登记"
    echo " 14) 诊断配置、端口、证书和站点"
    echo " 15) 修复伪装站权限（403）"
    echo "  0) 返回"
    read -rp "请选择: " choice || return 0
    case "$choice" in
      1) run_menu_command status "$INSTANCE_ID" ;;
      2) run_menu_command logs "$INSTANCE_ID" ;;
      3) run_menu_command follow "$INSTANCE_ID" ;;
      4) run_menu_command start "$INSTANCE_ID" ;;
      5) run_menu_command stop "$INSTANCE_ID" ;;
      6) run_menu_command restart "$INSTANCE_ID" ;;
      7) run_menu_command enable "$INSTANCE_ID" ;;
      8) run_menu_command disable "$INSTANCE_ID" ;;
      9) run_menu_command links "$INSTANCE_ID" ;;
      10) run_menu_command upstream "$INSTANCE_ID" ;;
      11) run_menu_command edit "$INSTANCE_ID" ;;
      12) run_menu_command credentials "$INSTANCE_ID" ;;
      13) run_menu_command delete "$INSTANCE_ID" ;;
      14) run_menu_command diagnose "$INSTANCE_ID" ;;
      15) run_menu_command repair-site "$INSTANCE_ID" ;;
      0) return 0 ;;
      *) yellow "请选择菜单中的编号" ;;
    esac
  done
}

manager_menu() {
  local choice
  while true; do
    echo
    blue "VLESS 实例管理"
    echo "  1) 安装落地 VLESS（直接出网）"
    echo "  2) 新增租户入口 VLESS（粘贴落地配置）"
    echo "  3) 列出所有实例和绑定"
    echo "  4) 管理租户（查看链接 / 修改 / 重置 / 删除）"
    echo "  5) 导入旧版部署"
    echo "  6) 查看租户 VLESS 链接和 Clash / Mihomo 节点"
    echo "  7) 选择租户进行一键诊断"
    echo "  0) 退出"
    read -rp "请选择: " choice || return 0
    case "$choice" in
      1) run_menu_command install direct ;;
      2) run_menu_command install relay ;;
      3) list_instances ;;
      4) if select_instance; then run_menu_command manage "$SELECTED_INSTANCE"; fi ;;
      5)
        local source service id
        read -rp "旧配置目录 [默认 /etc/xray-chain]: " source || return 0
        read -rp "旧服务名 [默认 xray-chain]: " service || return 0
        read -rp "导入后租户编号: " id || return 0
        run_menu_command import "$id" "${source:-/etc/xray-chain}" "${service:-xray-chain}"
        ;;
      6) if select_instance; then run_menu_command links "$SELECTED_INSTANCE"; fi ;;
      7) if select_instance; then run_menu_command diagnose "$SELECTED_INSTANCE"; fi ;;
      0) return 0 ;;
      *) yellow "请选择菜单中的编号" ;;
    esac
  done
}

install_instance() {
  NODE_MODE="$(normalize_node_mode "${1:-$NODE_MODE}")" || die "选择 direct 或 relay"
  INSTANCE_ID="${2:-$INSTANCE_ID}"
  if [[ -z "$INSTANCE_ID" ]]; then
    read -rp "租户编号（字母、数字、_、-）: " INSTANCE_ID || die "已取消"
  fi
  validate_instance_id "$INSTANCE_ID"
  install_base_tools
  manager_lock
  [[ ! -e "$MANAGER_DIR/instances/$INSTANCE_ID" ]] || die "租户已存在：$INSTANCE_ID"
  APP_DIR="$MANAGER_DIR/instances/$INSTANCE_ID"
  WEB_ROOT="$MANAGER_WEB_ROOT/instances/$INSTANCE_ID"
  SERVICE_NAME="xray-chain-$INSTANCE_ID"
  INSTANCE_IMPORTED=false
  safe_path "$APP_DIR"; safe_path "$WEB_ROOT"; safe_path "$XRAY_BIN"
  [[ ! -e "$WEB_ROOT" ]] || die "网站目录已存在：$WEB_ROOT"
  [[ ! -e "$SYSTEMD_DIR/$SERVICE_NAME.service" && ! -e "$OPENRC_DIR/$SERVICE_NAME" ]] ||
    die "服务已存在：$SERVICE_NAME"
  if [[ -z "$INSTANCE_NAME" && "$INTERACTIVE" == 1 ]]; then
    read -rp "租户显示名称 [默认 $INSTANCE_ID]: " INSTANCE_NAME || die "已取消"
  fi
  [[ -n "$INSTANCE_NAME" ]] || INSTANCE_NAME="$INSTANCE_ID"
  [[ ! "$INSTANCE_NAME" =~ [[:cntrl:]] ]] || die "名称不能包含控制字符"
  ask_domain_and_role
  ask_reality_target
  ask_cert_mode
  choose_public_ports
  ask_upstream
  warn_dns
  [[ ! -e "$(nginx_conf_path)" && ! -e "$RENEW_HOOK_DIR/$SERVICE_NAME.sh" ]] ||
    die "该实例的 Nginx 配置或续期脚本已存在"
  INSTALL_PENDING=1
  INSTALL_APP_CREATED=1
  prepare_dirs
  chmod 755 "$MANAGER_WEB_ROOT" "$MANAGER_WEB_ROOT/instances" "$WEB_ROOT"
  install_xray_if_needed
  install_nginx_if_needed
  install_certbot_for_mode
  choose_internal_ports
  configure_selinux
  if needs_nginx; then detect_preexisting_nginx_domain; fi
  if [[ -z "$UUID" ]]; then UUID="$("$XRAY_BIN" uuid)"; fi
  [[ "$UUID" =~ ^[0-9a-fA-F]{8}(-[0-9a-fA-F]{4}){3}-[0-9a-fA-F]{12}$ ]] || die "UUID 格式不正确"
  native_helper unique
  if [[ "$CERT_MODE" == http ]]; then write_nginx_challenge_config; fi
  verify_cf_token
  obtain_or_copy_cert
  write_final_nginx_config
  generate_reality_keys
  write_xray_config
  validate_xray_config
  write_systemd_service
  check_upstream_reachability
  start_xray
  setup_cert_renew_hook
  write_client_info
  save_install_env
  native_helper state-save "$(record_path)"
  INSTALL_PENDING=0
  show_result
}

main() {
  case "${1:-}" in -h|--help|help) usage; return 0 ;; esac
  [[ "$(id -u)" -eq 0 ]] || die "请使用 root 用户执行"
  umask 077
  trap cleanup EXIT
  trap 'exit 130' INT
  trap 'exit 143' TERM
  detect_platform
  local action="${1:-menu}"
  case "$action" in
    menu) manager_menu ;;
    install) install_instance "${2:-}" "${3:-}" ;;
    import) import_instance "${2:-}" "${3:-/etc/xray-chain}" "${4:-xray-chain}" ;;
    list) list_instances ;;
    manage)
      if [[ -n "${2:-}" ]]; then instance_menu "$2"
      elif select_instance; then instance_menu "$SELECTED_INSTANCE"
      fi ;;
    upstream) replace_upstream "${2:-}" ;;
    edit) edit_instance_menu "${2:-}" ;;
    credentials) credentials_menu "${2:-}" ;;
    rename) rename_instance "${2:-}" ;;
    ports) edit_instance_ports "${2:-}" ;;
    reset) reset_instance_credentials "${2:-}" "${3:-uuid}" ;;
    refresh-client) refresh_client_info "${2:-}" ;;
    delete) delete_instance "${2:-}" ;;
    diagnose) diagnose_instance "${2:-}" ;;
    repair-site) repair_instance_site "${2:-}" ;;
    status|logs|follow|start|stop|restart|enable|disable|links)
      load_instance "${2:-}"
      case "$action" in
        status) show_instance ;;
        logs) service_logs ;;
        follow) service_logs true ;;
        links) show_client_info ;;
        *) manager_lock; service_action "$action"; green "$INSTANCE_ID：$action 已执行" ;;
      esac ;;
    *) usage; die "未知命令：$action" ;;
  esac
}

if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
  SCRIPT_PATH="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)/$(basename -- "${BASH_SOURCE[0]}")"
  main "$@"
fi
