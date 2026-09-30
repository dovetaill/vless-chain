#!/usr/bin/env bash
set -Eeuo pipefail

# Native VLESS / Xray chain installer for Debian / Ubuntu.
# No Docker. Reuses an existing Nginx installation when available.

APP_DIR="${APP_DIR:-/etc/xray-chain}"
WEB_ROOT="${WEB_ROOT:-/var/www/xray-chain}"
XRAY_BIN="${XRAY_BIN:-/usr/local/bin/xray}"
SERVICE_NAME="${SERVICE_NAME:-xray-chain}"
XRAY_INSTALL_URL="${XRAY_INSTALL_URL:-https://github.com/XTLS/Xray-install/raw/main/install-release.sh}"

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
UPSTREAM_FLOW="${UPSTREAM_FLOW:-xtls-rprx-vision}"
UPSTREAM_FINGERPRINT="${UPSTREAM_FINGERPRINT:-chrome}"
UPSTREAM_ALLOW_INSECURE="${UPSTREAM_ALLOW_INSECURE:-false}"
UPSTREAM_REALITY_PUBLIC_KEY="${UPSTREAM_REALITY_PUBLIC_KEY:-}"
UPSTREAM_REALITY_SHORT_ID="${UPSTREAM_REALITY_SHORT_ID:-}"
UPSTREAM_REALITY_MLDSA65_VERIFY="${UPSTREAM_REALITY_MLDSA65_VERIFY:-}"

INTERNAL_HTTP_PORT="${INTERNAL_HTTP_PORT:-18080}"
INTERNAL_HTTPS_PORT="${INTERNAL_HTTPS_PORT:-18443}"

INTERACTIVE=1
[[ -t 0 ]] || INTERACTIVE=0
NGINX_DOMAIN_PREEXISTED=0

red() { printf '\033[31m%s\033[0m\n' "$*"; }
green() { printf '\033[32m%s\033[0m\n' "$*"; }
yellow() { printf '\033[33m%s\033[0m\n' "$*"; }
blue() { printf '\033[36m%s\033[0m\n' "$*"; }
die() { red "错误：$*"; exit 1; }

usage() {
  cat <<USAGE
用法：
  bash $0

本脚本不使用 Docker，适配 Debian / Ubuntu + systemd。

节点角色：
  NODE_MODE=direct   普通/落地节点：VLESS 入站 -> freedom -> Internet
  NODE_MODE=relay    中转节点：VLESS 入站 -> 下一跳 VLESS -> Internet

入口模式：
  SECURITY_MODE=tls      VLESS + TLS + Vision
  SECURITY_MODE=reality  VLESS + REALITY + Vision
  SECURITY_MODE=dual     REALITY + Vision 主入口 + TLS + Vision 备用入口

证书方式（仅需要本地证书时）：
  CERT_MODE=http         Certbot HTTP-01，经 Nginx 验证；不依赖 DNS 服务商；公网 80 必须可访问
  CERT_MODE=cloudflare   Certbot Cloudflare DNS-01；需要 CF_API_TOKEN；不要求公网 80
  CERT_MODE=existing     使用已有证书；填写 CERT_FILE / KEY_FILE
  CERT_MODE=none         仅 REALITY + remote target 等不需要本地证书的场景

默认端口：
  tls:      443
  reality:  443
  dual:     REALITY=443，TLS=8443

说明：
  - 选择 CERT_MODE=http 时，脚本仍默认优先使用 443 作为代理公网端口；
    但 Let's Encrypt HTTP-01 真正要求的是公网 80 可达。
  - 已安装 Nginx 时直接复用；未安装时 apt-get install -y nginx。
  - 如果 443 已被已有 Nginx/其它服务占用，脚本不会自动破坏现有网站，
    交互模式会要求换一个代理端口；非交互模式直接报错。
  - REALITY local target 使用本机 Nginx 内部 HTTPS 站点。
  - relay 推荐直接填写 UPSTREAM_URI，可自动解析 TLS / REALITY VLESS 分享链接。

示例：
  # B：普通落地节点，非 Cloudflare DNS，Certbot HTTP-01，REALITY 主入口
  NODE_MODE=direct SECURITY_MODE=reality CERT_MODE=http \\
  DOMAIN=node-01.docker.click EMAIL=you@example.com bash $0

  # A：中转节点，入口 REALITY，把流量交给 B
  NODE_MODE=relay SECURITY_MODE=reality CERT_MODE=http \\
  DOMAIN=edge-01.docker.click EMAIL=you@example.com \\
  UPSTREAM_URI='vless://...' bash $0

  # Cloudflare DNS-01
  NODE_MODE=direct SECURITY_MODE=dual CERT_MODE=cloudflare \\
  DOMAIN=node-01.docker.click EMAIL=you@example.com \\
  CF_API_TOKEN='TOKEN' bash $0
USAGE
}

if [[ "${1:-}" == "-h" || "${1:-}" == "--help" ]]; then
  usage
  exit 0
fi

[[ "$(id -u)" -eq 0 ]] || die "请使用 root 用户执行"

need_cmd() { command -v "$1" >/dev/null 2>&1; }

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
  check_domain "$h" || check_ipv4 "$h" || [[ "$h" == *:* ]]
}

check_port_number() {
  local p="$1"
  [[ "$p" =~ ^[0-9]+$ ]] || return 1
  (( p >= 1 && p <= 65535 ))
}

port_in_use() {
  local p="$1"
  ss -lnt 2>/dev/null | awk '{print $4}' | grep -Eq "(:|\\])${p}$"
}

port_owned_by_nginx() {
  local p="$1"
  ss -lntp 2>/dev/null | grep -E "(:|\\])${p}[[:space:]]" | grep -q 'nginx'
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
  local uri="$1" rest authority hostport query transport encryption
  [[ "$uri" == vless://* ]] || die "上游链接必须以 vless:// 开头"

  rest="${uri#vless://}"
  authority="${rest%%\?*}"
  [[ "$authority" != "$rest" ]] || die "上游 VLESS 链接缺少查询参数"
  query="${rest#*\?}"
  query="${query%%#*}"

  [[ "$authority" == *@* ]] || die "上游链接缺少 UUID 或地址"
  UPSTREAM_UUID="${authority%%@*}"
  hostport="${authority#*@}"

  if [[ "$hostport" =~ ^\[([^]]+)\]:([0-9]+)$ ]]; then
    UPSTREAM_ADDRESS="${BASH_REMATCH[1]}"
    UPSTREAM_PORT="${BASH_REMATCH[2]}"
  else
    [[ "$hostport" == *:* ]] || die "上游 VLESS 链接缺少端口"
    UPSTREAM_ADDRESS="${hostport%:*}"
    UPSTREAM_PORT="${hostport##*:}"
  fi

  UPSTREAM_SECURITY="$(query_param "$query" security || true)"
  transport="$(query_param "$query" type || true)"
  encryption="$(query_param "$query" encryption || true)"
  UPSTREAM_SNI="$(query_param "$query" sni || true)"
  UPSTREAM_FLOW="$(query_param "$query" flow || true)"
  UPSTREAM_FINGERPRINT="$(query_param "$query" fp || true)"
  UPSTREAM_REALITY_PUBLIC_KEY="$(query_param "$query" pbk || true)"
  UPSTREAM_REALITY_SHORT_ID="$(query_param "$query" sid || true)"
  UPSTREAM_REALITY_MLDSA65_VERIFY="$(query_param "$query" pqv || true)"

  [[ -n "$UPSTREAM_SECURITY" ]] || UPSTREAM_SECURITY=tls
  [[ -n "$transport" ]] || transport=tcp
  [[ -n "$encryption" ]] || encryption=none
  [[ -n "$UPSTREAM_FLOW" ]] || UPSTREAM_FLOW=xtls-rprx-vision
  [[ -n "$UPSTREAM_FINGERPRINT" ]] || UPSTREAM_FINGERPRINT=chrome
  [[ -n "$UPSTREAM_SNI" ]] || UPSTREAM_SNI="$UPSTREAM_ADDRESS"

  [[ "$transport" == tcp || "$transport" == raw ]] || die "当前链式模式只支持 type=tcp/raw"
  [[ "$encryption" == none ]] || die "上游 encryption 必须为 none"
  [[ "$UPSTREAM_SECURITY" == tls || "$UPSTREAM_SECURITY" == reality ]] || die "上游仅支持 TLS / REALITY"
}

validate_upstream() {
  check_host "$UPSTREAM_ADDRESS" || die "上游地址格式不正确：$UPSTREAM_ADDRESS"
  check_port_number "$UPSTREAM_PORT" || die "上游端口格式不正确：$UPSTREAM_PORT"
  [[ "$UPSTREAM_UUID" =~ ^[0-9a-fA-F-]{36}$ ]] || die "上游 UUID 格式不正确"
  [[ "$UPSTREAM_FLOW" == xtls-rprx-vision || "$UPSTREAM_FLOW" == xtls-rprx-vision-udp443 ]] || die "当前只支持 Vision flow"

  if [[ "$UPSTREAM_SECURITY" == reality ]]; then
    [[ -n "$UPSTREAM_REALITY_PUBLIC_KEY" ]] || die "REALITY 上游缺少 pbk"
    validate_short_id "$UPSTREAM_REALITY_SHORT_ID" || die "REALITY 上游 sid 格式不正确"
  fi
}

repair_dpkg_state() {
  if need_cmd dpkg && dpkg --audit 2>/dev/null | grep -q .; then
    yellow "检测到 dpkg 状态异常，尝试修复..."
    dpkg --configure -a >/dev/null 2>&1 || true
    apt-get -f install -y >/dev/null 2>&1 || true
  fi
}

install_base_tools() {
  need_cmd apt-get || die "当前版本仅适配 Debian / Ubuntu"
  repair_dpkg_state

  local packages=(ca-certificates curl openssl iproute2 coreutils)
  local missing=0 cmd
  for cmd in curl openssl ss timeout; do
    need_cmd "$cmd" || missing=1
  done

  if [[ "$missing" == 1 ]]; then
    blue "安装基础依赖..."
    apt-get update -y
    DEBIAN_FRONTEND=noninteractive apt-get install -y "${packages[@]}"
  else
    green "基础依赖已满足"
  fi
}

install_xray_if_needed() {
  if [[ -x "$XRAY_BIN" ]]; then
    green "检测到 Xray：$($XRAY_BIN version 2>/dev/null | head -n1 || true)"
    return 0
  fi

  blue "未检测到 Xray，使用 XTLS 官方安装脚本安装..."
  local tmp
  tmp="$(mktemp)"
  curl -fsSL "$XRAY_INSTALL_URL" -o "$tmp" || die "下载 Xray 官方安装脚本失败"
  bash "$tmp" install --without-geodata -u root
  rm -f "$tmp"
  [[ -x "$XRAY_BIN" ]] || die "Xray 安装失败"

  # The official installer creates xray.service. This script uses an isolated
  # xray-chain.service and therefore disables only the newly installed default service.
  systemctl disable --now xray.service >/dev/null 2>&1 || true
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
    blue "未检测到 Nginx，使用 apt 安装..."
    apt-get update -y
    DEBIAN_FRONTEND=noninteractive apt-get install -y nginx
  fi

  systemctl enable nginx >/dev/null 2>&1 || true
}

install_certbot_for_mode() {
  [[ "$CERT_MODE" == http || "$CERT_MODE" == cloudflare ]] || return 0

  blue "检查 Certbot..."
  apt-get update -y
  if [[ "$CERT_MODE" == http ]]; then
    DEBIAN_FRONTEND=noninteractive apt-get install -y certbot python3-certbot-nginx
  else
    DEBIAN_FRONTEND=noninteractive apt-get install -y certbot python3-certbot-dns-cloudflare
  fi
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
    REALITY_TARGET="${SPLIT_HOST}:${SPLIT_PORT}"
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
  while port_in_use "$current"; do
    yellow "${label} 端口 ${current} 已被占用。"
    if [[ "$INTERACTIVE" != 1 ]]; then
      die "端口 ${current} 已占用；请显式设置其它端口后重试"
    fi
    read -rp "请输入新的 ${label} 端口 [默认 ${fallback}]: " current
    [[ -n "$current" ]] || current="$fallback"
    check_port_number "$current" || { yellow "端口格式不正确"; current="$fallback"; }
    fallback=$((current + 1))
  done
  printf '%s' "$current"
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

  if [[ -n "$UPSTREAM_URI" ]]; then
    parse_upstream_uri "$UPSTREAM_URI"
  elif [[ -n "$UPSTREAM_ADDRESS" && -n "$UPSTREAM_PORT" && -n "$UPSTREAM_UUID" ]]; then
    [[ -n "$UPSTREAM_SECURITY" ]] || UPSTREAM_SECURITY=tls
    [[ -n "$UPSTREAM_SNI" ]] || UPSTREAM_SNI="$UPSTREAM_ADDRESS"
  else
    read -rp "请粘贴下一跳 vless:// 分享链接: " UPSTREAM_URI
    parse_upstream_uri "$UPSTREAM_URI"
  fi

  validate_upstream
  green "下一跳：${UPSTREAM_ADDRESS}:${UPSTREAM_PORT} (${UPSTREAM_SECURITY})"
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
}

find_free_internal_port() {
  local p="$1"
  while port_in_use "$p"; do p=$((p + 1)); done
  printf '%s' "$p"
}

choose_internal_ports() {
  needs_nginx || return 0
  INTERNAL_HTTP_PORT="$(find_free_internal_port "$INTERNAL_HTTP_PORT")"
  INTERNAL_HTTPS_PORT="$(find_free_internal_port "$INTERNAL_HTTPS_PORT")"
  [[ "$INTERNAL_HTTP_PORT" != "$INTERNAL_HTTPS_PORT" ]] || INTERNAL_HTTPS_PORT=$((INTERNAL_HTTPS_PORT + 1))

  if uses_reality && [[ "$REALITY_TARGET_MODE" == local ]]; then
    REALITY_TARGET="127.0.0.1:${INTERNAL_HTTPS_PORT}"
    REALITY_SERVER_NAME="$DOMAIN"
  fi
}

nginx_conf_path() {
  printf '/etc/nginx/conf.d/xray-chain-%s.conf' "$DOMAIN"
}

detect_preexisting_nginx_domain() {
  local own_conf
  own_conf="$(nginx_conf_path)"

  if grep -RhsE \
    --exclude="$(basename "$own_conf")" \
    "server_name[[:space:]][^;]*${DOMAIN//./\\.}([[:space:];]|$)" \
    /etc/nginx/conf.d /etc/nginx/sites-enabled 2>/dev/null | grep -q .; then
    NGINX_DOMAIN_PREEXISTED=1
  else
    NGINX_DOMAIN_PREEXISTED=0
  fi
}

write_nginx_challenge_config() {
  [[ "$CERT_MODE" == http ]] || return 0
  local conf
  conf="$(nginx_conf_path)"
  mkdir -p /etc/nginx/conf.d

  local public80=""
  if [[ "$NGINX_DOMAIN_PREEXISTED" == 1 ]]; then
    yellow "Nginx 已存在 ${DOMAIN} 的 server_name，Certbot 将复用现有站点进行 HTTP-01 验证。"
  else
    public80=$(cat <<NGINX
server {
    listen 80;
    listen [::]:80;
    server_name ${DOMAIN};
    root ${WEB_ROOT};

    location ^~ /.well-known/acme-challenge/ {
        try_files \$uri =404;
    }

    location / {
        try_files \$uri \$uri/ /index.html;
    }
}
NGINX
)
  fi

  cat > "$conf" <<NGINX
# Managed by xray-vless-native-v3.sh
${public80}
server {
    listen 127.0.0.1:${INTERNAL_HTTP_PORT};
    server_name ${DOMAIN};
    root ${WEB_ROOT};
    index index.html;
    location / { try_files \$uri \$uri/ /index.html; }
}
NGINX

  nginx -t || die "Nginx challenge 配置检查失败"
  systemctl restart nginx
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
  [[ "$status" == 200 ]] && echo "$body" | grep -q '"success"[[:space:]]*:[[:space:]]*true' || die "Cloudflare Token 校验失败"
  green "Cloudflare Token 有效"
}

obtain_or_copy_cert() {
  needs_local_cert || return 0
  local le_live="/etc/letsencrypt/live/${DOMAIN}"

  if [[ "$CERT_MODE" == http ]]; then
    blue "使用 Certbot + Nginx 执行 HTTP-01 证书申请..."
    certbot certonly --nginx \
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
    cat > "$APP_DIR/cloudflare.ini" <<CF
# Managed by xray-vless-native-v3.sh
dns_cloudflare_api_token = ${CF_API_TOKEN}
CF
    chmod 600 "$APP_DIR/cloudflare.ini"
    certbot certonly \
      --dns-cloudflare \
      --dns-cloudflare-credentials "$APP_DIR/cloudflare.ini" \
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

write_final_nginx_config() {
  needs_nginx || return 0
  local conf public80="" primary_port
  conf="$(nginx_conf_path)"
  primary_port="$(primary_public_port)"

  if [[ "$CERT_MODE" == http && "$NGINX_DOMAIN_PREEXISTED" != 1 ]]; then
    local redirect_target
    if [[ "$primary_port" == 443 ]]; then
      redirect_target='https://$host$request_uri'
    else
      redirect_target="https://\$host:${primary_port}\$request_uri"
    fi

    public80=$(cat <<NGINX
server {
    listen 80;
    listen [::]:80;
    server_name ${DOMAIN};
    root ${WEB_ROOT};

    location ^~ /.well-known/acme-challenge/ {
        try_files \$uri =404;
    }

    location / {
        return 301 ${redirect_target};
    }
}
NGINX
)
  fi

  cat > "$conf" <<NGINX
# Managed by xray-vless-native-v3.sh
${public80}
# TLS fallback after Xray has terminated TLS.
server {
    listen 127.0.0.1:${INTERNAL_HTTP_PORT};
    server_name ${DOMAIN};
    root ${WEB_ROOT};
    index index.html;
    location / { try_files \$uri \$uri/ /index.html; }
}

# REALITY local target. This listener performs a real local TLS handshake.
server {
    listen 127.0.0.1:${INTERNAL_HTTPS_PORT} ssl;
    server_name ${DOMAIN};
    ssl_certificate ${APP_DIR}/certs/fullchain.pem;
    ssl_certificate_key ${APP_DIR}/certs/privkey.pem;
    ssl_protocols TLSv1.2 TLSv1.3;
    root ${WEB_ROOT};
    index index.html;
    location / { try_files \$uri \$uri/ /index.html; }
}
NGINX

  nginx -t || die "Nginx 最终配置检查失败"
  systemctl restart nginx
  green "Nginx 配置完成"
}

generate_reality_keys() {
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

  cat > "$APP_DIR/reality.env" <<ENV
REALITY_PRIVATE_KEY=${REALITY_PRIVATE_KEY}
REALITY_PUBLIC_KEY=${REALITY_PUBLIC_KEY}
REALITY_SHORT_ID=${REALITY_SHORT_ID}
REALITY_TARGET=${REALITY_TARGET}
REALITY_SERVER_NAME=${REALITY_SERVER_NAME}
ENV
  chmod 600 "$APP_DIR/reality.env"
}

build_upstream_outbounds() {
  if [[ "$NODE_MODE" == direct ]]; then
    cat <<JSON
    {
      "tag": "direct",
      "protocol": "freedom"
    }
JSON
    return 0
  fi

  if [[ "$UPSTREAM_SECURITY" == tls ]]; then
    cat <<JSON
    {
      "tag": "upstream-vless",
      "protocol": "vless",
      "settings": {
        "address": "${UPSTREAM_ADDRESS}",
        "port": ${UPSTREAM_PORT},
        "id": "${UPSTREAM_UUID}",
        "encryption": "none",
        "flow": "${UPSTREAM_FLOW}"
      },
      "streamSettings": {
        "method": "raw",
        "security": "tls",
        "tlsSettings": {
          "serverName": "${UPSTREAM_SNI}",
          "allowInsecure": ${UPSTREAM_ALLOW_INSECURE},
          "fingerprint": "${UPSTREAM_FINGERPRINT}"
        }
      }
    },
    {
      "tag": "direct",
      "protocol": "freedom"
    }
JSON
  else
    local pq=""
    if [[ -n "$UPSTREAM_REALITY_MLDSA65_VERIFY" ]]; then
      pq=",\n          \"mldsa65Verify\": \"${UPSTREAM_REALITY_MLDSA65_VERIFY}\""
    fi
    cat <<JSON
    {
      "tag": "upstream-vless",
      "protocol": "vless",
      "settings": {
        "address": "${UPSTREAM_ADDRESS}",
        "port": ${UPSTREAM_PORT},
        "id": "${UPSTREAM_UUID}",
        "encryption": "none",
        "flow": "${UPSTREAM_FLOW}"
      },
      "streamSettings": {
        "method": "raw",
        "security": "reality",
        "realitySettings": {
          "serverName": "${UPSTREAM_SNI}",
          "fingerprint": "${UPSTREAM_FINGERPRINT}",
          "password": "${UPSTREAM_REALITY_PUBLIC_KEY}",
          "shortId": "${UPSTREAM_REALITY_SHORT_ID}"${pq}
        }
      }
    },
    {
      "tag": "direct",
      "protocol": "freedom"
    }
JSON
  fi
}

write_xray_config() {
  blue "写入 Xray 配置..."
  local inbounds="" inbound_tags="" route_outbound outbounds
  route_outbound=direct
  [[ "$NODE_MODE" == relay ]] && route_outbound=upstream-vless

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
    "loglevel": "warning"
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
  cat > "/etc/systemd/system/${SERVICE_NAME}.service" <<SERVICE
[Unit]
Description=Xray VLESS Chain Service
Documentation=https://github.com/XTLS/Xray-core
After=network-online.target nss-lookup.target
Wants=network-online.target

[Service]
Type=simple
User=root
ExecStart=${XRAY_BIN} run -config ${APP_DIR}/config.json
Restart=on-failure
RestartSec=3s
LimitNOFILE=1000000

[Install]
WantedBy=multi-user.target
SERVICE

  systemctl daemon-reload
}

validate_xray_config() {
  blue "校验 Xray 配置..."
  "$XRAY_BIN" run -test -config "$APP_DIR/config.json" >/dev/null
  green "Xray 配置校验通过"
}

check_upstream_reachability() {
  [[ "$NODE_MODE" == relay ]] || return 0
  blue "检查下一跳 TCP ${UPSTREAM_ADDRESS}:${UPSTREAM_PORT}..."
  if timeout 6 bash -c "</dev/tcp/${UPSTREAM_ADDRESS}/${UPSTREAM_PORT}" 2>/dev/null; then
    green "下一跳端口可达"
  else
    yellow "下一跳当前不可达；保留配置，但 relay 启动后可能无法正常转发。"
  fi
}

start_xray() {
  systemctl enable --now "$SERVICE_NAME"
  sleep 2
  if ! systemctl is-active --quiet "$SERVICE_NAME"; then
    journalctl -u "$SERVICE_NAME" -n 40 --no-pager || true
    die "Xray 服务启动失败"
  fi
  green "${SERVICE_NAME} 已启动"
}

setup_cert_renew_hook() {
  [[ "$CERT_MODE" == http || "$CERT_MODE" == cloudflare ]] || return 0
  mkdir -p /etc/letsencrypt/renewal-hooks/deploy
  cat > /etc/letsencrypt/renewal-hooks/deploy/xray-chain-reload.sh <<HOOK
#!/usr/bin/env bash
set -Eeuo pipefail
DOMAIN='${DOMAIN}'
APP_DIR='${APP_DIR}'
cp -L "/etc/letsencrypt/live/\${DOMAIN}/fullchain.pem" "\${APP_DIR}/certs/fullchain.pem"
cp -L "/etc/letsencrypt/live/\${DOMAIN}/privkey.pem" "\${APP_DIR}/certs/privkey.pem"
chmod 644 "\${APP_DIR}/certs/fullchain.pem"
chmod 600 "\${APP_DIR}/certs/privkey.pem"
nginx -t && systemctl reload nginx
systemctl restart '${SERVICE_NAME}'
HOOK
  chmod 700 /etc/letsencrypt/renewal-hooks/deploy/xray-chain-reload.sh
  systemctl enable --now certbot.timer >/dev/null 2>&1 || true
}

write_client_info() {
  local tls_uri reality_uri mode_text
  if [[ "$NODE_MODE" == relay ]]; then
    mode_text="relay -> ${UPSTREAM_SECURITY}://${UPSTREAM_ADDRESS}:${UPSTREAM_PORT}"
  else
    mode_text="direct -> Internet"
  fi

  : > "$APP_DIR/client.txt"
  {
    echo "节点角色: ${mode_text}"
    echo "入口域名: ${DOMAIN}"
    echo "UUID: ${UUID}"
    echo "证书模式: ${CERT_MODE}"
    echo

    if uses_reality; then
      reality_uri="vless://${UUID}@${DOMAIN}:${REALITY_PORT}?encryption=none&security=reality&type=tcp&sni=${REALITY_SERVER_NAME}&fp=${REALITY_FINGERPRINT}&pbk=${REALITY_PUBLIC_KEY}&sid=${REALITY_SHORT_ID}&flow=xtls-rprx-vision#${DOMAIN}-REALITY-Vision"
      echo "[推荐] VLESS + REALITY + Vision"
      echo "端口: ${REALITY_PORT}"
      echo "SNI: ${REALITY_SERVER_NAME}"
      echo "PublicKey/Password: ${REALITY_PUBLIC_KEY}"
      echo "ShortId: ${REALITY_SHORT_ID}"
      echo "分享链接:"
      echo "$reality_uri"
      echo
    fi

    if uses_tls; then
      tls_uri="vless://${UUID}@${DOMAIN}:${TLS_PORT}?encryption=none&security=tls&type=tcp&sni=${DOMAIN}&fp=chrome&flow=xtls-rprx-vision#${DOMAIN}-TLS-Vision"
      echo "VLESS + TLS + Vision"
      echo "端口: ${TLS_PORT}"
      echo "SNI: ${DOMAIN}"
      echo "分享链接:"
      echo "$tls_uri"
      echo
    fi
  } >> "$APP_DIR/client.txt"
  chmod 600 "$APP_DIR/client.txt"
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
  green "安装完成"
  blue "Xray 配置：${APP_DIR}/config.json"
  blue "客户端信息：${APP_DIR}/client.txt"
  blue "Nginx 配置：$(nginx_conf_path)"
  blue "systemd 服务：${SERVICE_NAME}.service"
  echo
  cat "$APP_DIR/client.txt"
  echo
  yellow "管理命令："
  echo "  systemctl status ${SERVICE_NAME}"
  echo "  journalctl -u ${SERVICE_NAME} -f"
  echo "  systemctl restart ${SERVICE_NAME}"
  if needs_nginx; then
    echo "  nginx -t && systemctl reload nginx"
  fi
  if [[ "$CERT_MODE" == http || "$CERT_MODE" == cloudflare ]]; then
    echo "  certbot renew --dry-run"
  fi
  echo
  if [[ "$CERT_MODE" == http ]]; then
    yellow "HTTP-01 使用公网 80 做证书验证；代理入口默认优先 443，但证书本身并不强制只能用于 443。"
  fi
  yellow "脚本不会自动接管已经被其它服务占用的 443，以避免破坏已有网站。"
}

main() {
  install_base_tools
  ask_domain_and_role
  ask_reality_target
  ask_cert_mode

  # Stop only our own previous instance before checking public ports.
  systemctl stop "$SERVICE_NAME" >/dev/null 2>&1 || true

  choose_public_ports
  ask_upstream
  warn_dns
  prepare_dirs

  install_xray_if_needed
  install_nginx_if_needed
  install_certbot_for_mode
  choose_internal_ports
  if needs_nginx; then
    detect_preexisting_nginx_domain
  fi

  if [[ -z "$UUID" ]]; then UUID="$($XRAY_BIN uuid)"; fi
  [[ "$UUID" =~ ^[0-9a-fA-F-]{36}$ ]] || die "UUID 格式不正确：$UUID"

  if [[ "$CERT_MODE" == http ]]; then
    write_nginx_challenge_config
  fi

  verify_cf_token
  obtain_or_copy_cert
  write_final_nginx_config
  generate_reality_keys
  write_xray_config
  write_systemd_service
  validate_xray_config
  check_upstream_reachability
  start_xray
  setup_cert_renew_hook
  write_client_info
  save_install_env
  show_result
}

main "$@"

