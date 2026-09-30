#!/usr/bin/env bash
set -Eeuo pipefail

APP_DIR="${APP_DIR:-/opt/xray-docker}"
DOMAIN="${DOMAIN:-}"
EMAIL="${EMAIL:-}"
UUID="${UUID:-}"
CF_API_TOKEN="${CF_API_TOKEN:-}"

NODE_MODE="${NODE_MODE:-}"
SECURITY_MODE="${SECURITY_MODE:-}"
LISTEN_PORT="${LISTEN_PORT:-}"
TLS_PORT="${TLS_PORT:-}"
REALITY_PORT="${REALITY_PORT:-}"

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

red() { echo -e "\033[31m$*\033[0m"; }
green() { echo -e "\033[32m$*\033[0m"; }
yellow() { echo -e "\033[33m$*\033[0m"; }
blue() { echo -e "\033[36m$*\033[0m"; }
die() { red "错误：$*"; exit 1; }

usage() {
  cat <<USAGE
用法:
  bash $0

支持的入口安全模式:
  SECURITY_MODE=tls      VLESS + TLS + Vision + 本地 HTTPS 伪装站
  SECURITY_MODE=reality  VLESS + REALITY + Vision
  SECURITY_MODE=dual     TLS Vision 与 REALITY Vision 同时启用（不同端口）

支持的节点角色:
  NODE_MODE=direct       最终落地：本机直接访问 Internet
  NODE_MODE=relay        中转节点：本机把流量继续交给下一台 VLESS

推荐：
  - 中国大陆客户端 -> A：优先 REALITY + Vision
  - A -> B：可用 TLS + Vision 或 REALITY + Vision
  - dual 模式下默认 REALITY_PORT=443，TLS_PORT=8443
  - REALITY_TARGET_MODE=local 时，普通浏览器访问 REALITY 端口会落到本机 HTTPS 伪装站

非交互示例：
  # B：双栈落地节点
  NODE_MODE=direct SECURITY_MODE=dual \
  DOMAIN=node-01.example.com EMAIL=you@example.com CF_API_TOKEN=xxx \
  REALITY_PORT=443 TLS_PORT=8443 bash $0

  # A：双栈入口，并把代理流量交给 B 的 REALITY 链接
  NODE_MODE=relay SECURITY_MODE=dual \
  DOMAIN=edge-01.example.com EMAIL=you@example.com CF_API_TOKEN=xxx \
  REALITY_PORT=443 TLS_PORT=8443 \
  UPSTREAM_URI='vless://UUID@node-01.example.com:443?encryption=none&security=reality&type=tcp&sni=node-01.example.com&fp=chrome&pbk=PUBLIC_KEY&sid=SHORT_ID&flow=xtls-rprx-vision' \
  bash $0

主要环境变量:
  APP_DIR=/opt/xray-docker
  DOMAIN=节点域名
  EMAIL=申请证书邮箱
  CF_API_TOKEN=Cloudflare API Token
  UUID=本机 VLESS UUID，可不填
  NODE_MODE=direct|relay
  SECURITY_MODE=tls|reality|dual
  LISTEN_PORT=兼容旧参数：tls/reality 单模式时作为该模式端口；dual 时作为 REALITY_PORT
  TLS_PORT=TLS Vision 端口
  REALITY_PORT=REALITY Vision 端口

REALITY:
  REALITY_TARGET_MODE=local|remote
  REALITY_TARGET=远程目标，例如 www.example.com:443（remote 模式）
  REALITY_SERVER_NAME=客户端 SNI，默认取 REALITY_TARGET 主机名
  REALITY_PRIVATE_KEY=可选，不填自动生成
  REALITY_PUBLIC_KEY=可选，不填自动生成/推导
  REALITY_SHORT_ID=可选，不填自动生成 8 字节十六进制
  REALITY_FINGERPRINT=chrome

relay 上游:
  推荐直接填 UPSTREAM_URI，可自动解析 TLS 或 REALITY VLESS 链接。

说明:
  - TLS 与 REALITY 不是叠加在同一个 inbound 上，而是两套独立入口。
  - dual 模式用于主备，不代表同一条连接同时套 TLS + REALITY。
  - 本脚本使用 RAW 传输（原 TCP transport 的新名称）+ xtls-rprx-vision。
  - 每台 relay 只保存自己的下一跳，支持 A -> B -> C -> ...。
USAGE
}

if [[ "${1:-}" == "-h" || "${1:-}" == "--help" ]]; then
  usage
  exit 0
fi

[[ "$(id -u)" -eq 0 ]] || die "请使用 root 用户执行"

need_cmd() { command -v "$1" >/dev/null 2>&1; }
compose() { docker compose "$@"; }

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
  [[ "$p" -ge 1 && "$p" -le 65535 ]] || return 1
}

normalize_node_mode() {
  local mode
  mode="$(echo "$1" | tr '[:upper:]' '[:lower:]' | tr -d '[:space:]')"
  case "$mode" in
    1|direct|exit|landing) echo direct ;;
    2|relay|chain|upstream) echo relay ;;
    *) return 1 ;;
  esac
}

normalize_security_mode() {
  local mode
  mode="$(echo "$1" | tr '[:upper:]' '[:lower:]' | tr -d '[:space:]')"
  case "$mode" in
    1|tls) echo tls ;;
    2|reality) echo reality ;;
    3|dual|both) echo dual ;;
    *) return 1 ;;
  esac
}

normalize_reality_target_mode() {
  local mode
  mode="$(echo "$1" | tr '[:upper:]' '[:lower:]' | tr -d '[:space:]')"
  case "$mode" in
    1|local|self) echo local ;;
    2|remote|external) echo remote ;;
    *) return 1 ;;
  esac
}

uses_tls() { [[ "$SECURITY_MODE" == tls || "$SECURITY_MODE" == dual ]]; }
uses_reality() { [[ "$SECURITY_MODE" == reality || "$SECURITY_MODE" == dual ]]; }
needs_cert() { uses_tls || { uses_reality && [[ "$REALITY_TARGET_MODE" == local ]]; }; }

query_param() {
  local query="$1" wanted="$2" pair key value old_ifs="$IFS"
  IFS='&'
  for pair in $query; do
    key="${pair%%=*}"
    value="${pair#*=}"
    if [[ "$key" == "$wanted" ]]; then
      IFS="$old_ifs"
      printf '%s' "$value"
      return 0
    fi
  done
  IFS="$old_ifs"
  return 1
}

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
    SPLIT_PORT="443"
  fi
}

parse_upstream_uri() {
  local uri="$1" rest authority hostport query transport encryption

  [[ "$uri" == vless://* ]] || die "上游链接必须以 vless:// 开头"
  rest="${uri#vless://}"
  authority="${rest%%\?*}"
  [[ "$authority" != "$rest" ]] || die "上游 VLESS 链接缺少查询参数"
  query="${rest#*\?}"
  query="${query%%#*}"

  [[ "$authority" == *@* ]] || die "上游 VLESS 链接缺少 UUID 或地址"
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

  [[ "$transport" == tcp || "$transport" == raw ]] || die "当前链式模式只支持 type=tcp/raw，上游实际为：$transport"
  [[ "$encryption" == none ]] || die "当前脚本期望上游 encryption=none"
  [[ "$UPSTREAM_SECURITY" == tls || "$UPSTREAM_SECURITY" == reality ]] || die "上游 security 仅支持 tls/reality，实际为：$UPSTREAM_SECURITY"

  if [[ -z "$UPSTREAM_SNI" && "$UPSTREAM_SECURITY" == tls ]]; then
    UPSTREAM_SNI="$UPSTREAM_ADDRESS"
  fi
}

validate_short_id() {
  local sid="$1"
  [[ "$sid" =~ ^[0-9a-fA-F]{0,16}$ ]] || return 1
  (( ${#sid} % 2 == 0 ))
}

validate_upstream() {
  check_host "$UPSTREAM_ADDRESS" || die "上游地址格式不正确：$UPSTREAM_ADDRESS"
  check_port_number "$UPSTREAM_PORT" || die "上游端口格式不正确：$UPSTREAM_PORT"
  [[ "$UPSTREAM_UUID" =~ ^[0-9a-fA-F-]{36}$ ]] || die "上游 UUID 格式不正确：$UPSTREAM_UUID"
  [[ "$UPSTREAM_FLOW" == xtls-rprx-vision || "$UPSTREAM_FLOW" == xtls-rprx-vision-udp443 ]] || die "当前脚本只支持 Vision flow"
  [[ "$UPSTREAM_FINGERPRINT" =~ ^[a-zA-Z0-9_-]+$ ]] || die "上游 fingerprint 格式不正确"

  if [[ "$UPSTREAM_SECURITY" == tls ]]; then
    [[ -n "$UPSTREAM_SNI" ]] || die "TLS 上游缺少 SNI"
    [[ "$UPSTREAM_ALLOW_INSECURE" == true || "$UPSTREAM_ALLOW_INSECURE" == false ]] || die "UPSTREAM_ALLOW_INSECURE 只能是 true 或 false"
  else
    [[ -n "$UPSTREAM_SNI" ]] || die "REALITY 上游缺少 sni"
    [[ -n "$UPSTREAM_REALITY_PUBLIC_KEY" ]] || die "REALITY 上游链接缺少 pbk"
    validate_short_id "$UPSTREAM_REALITY_SHORT_ID" || die "REALITY 上游 sid 必须为不超过 16 位且长度为偶数的十六进制"
  fi

  if [[ "$UPSTREAM_ADDRESS" == "$DOMAIN" ]]; then
    if { uses_tls && [[ "$UPSTREAM_PORT" == "$TLS_PORT" ]]; } || { uses_reality && [[ "$UPSTREAM_PORT" == "$REALITY_PORT" ]]; }; then
      die "下一跳指向了本机自己，会形成代理死循环"
    fi
  fi
}

port_in_use() {
  local port="$1"
  ss -lnt 2>/dev/null | awk '{print $4}' | grep -Eq "(:|\\])${port}$"
}

repair_dpkg_state() {
  if dpkg --audit 2>/dev/null | grep -q .; then
    yellow "检测到 dpkg 状态异常，尝试修复..."
    if dpkg --audit 2>/dev/null | grep -q docker-buildx-plugin; then
      dpkg --remove --force-remove-reinstreq docker-buildx-plugin >/dev/null 2>&1 || true
    fi
    dpkg --configure -a >/dev/null 2>&1 || true
    apt-get -f install -y >/dev/null 2>&1 || true
  fi
}

setup_docker_official_repo() {
  blue "配置 Docker 官方 APT 源..."
  apt-get update -y
  DEBIAN_FRONTEND=noninteractive apt-get install -y ca-certificates curl gnupg
  . /etc/os-release
  local os_id="${ID:-}" codename="${VERSION_CODENAME:-${UBUNTU_CODENAME:-}}"
  [[ -n "$os_id" && -n "$codename" ]] || die "无法识别系统版本"
  [[ "$os_id" == debian || "$os_id" == ubuntu ]] || die "当前脚本主要适配 Debian/Ubuntu"
  install -m 0755 -d /etc/apt/keyrings
  rm -f /etc/apt/keyrings/docker.gpg
  curl -fsSL "https://download.docker.com/linux/${os_id}/gpg" | gpg --dearmor -o /etc/apt/keyrings/docker.gpg
  chmod a+r /etc/apt/keyrings/docker.gpg
  cat > /etc/apt/sources.list.d/docker.list <<APT
deb [arch=$(dpkg --print-architecture) signed-by=/etc/apt/keyrings/docker.gpg] https://download.docker.com/linux/${os_id} ${codename} stable
APT
  apt-get update -y
}

install_base_tools() {
  local need_install=0
  blue "检查基础依赖..."
  for cmd in curl openssl ss crontab gpg timeout; do need_cmd "$cmd" || need_install=1; done
  [[ "$need_install" == 0 ]] && { green "基础依赖已满足"; return 0; }
  need_cmd apt-get || die "当前脚本主要适配 Debian/Ubuntu"
  repair_dpkg_state
  apt-get update -y
  DEBIAN_FRONTEND=noninteractive apt-get install -y ca-certificates curl cron iproute2 openssl gnupg coreutils
}

install_docker_if_needed() {
  if need_cmd docker; then
    green "Docker 已安装"
  else
    yellow "未检测到 Docker，开始安装..."
    repair_dpkg_state
    if ! (apt-get update -y && DEBIAN_FRONTEND=noninteractive apt-get install -y docker.io docker-compose-plugin); then
      setup_docker_official_repo
      DEBIAN_FRONTEND=noninteractive apt-get install -y docker-ce docker-ce-cli containerd.io docker-compose-plugin
    fi
  fi
  systemctl enable --now docker >/dev/null 2>&1 || true
  docker info >/dev/null 2>&1 || die "Docker daemon 未正常运行"
  docker compose version >/dev/null 2>&1 || die "Docker Compose v2 不可用"
}

ask_inputs() {
  if [[ -z "$DOMAIN" ]]; then read -rp "请输入节点域名，例如 edge-01.docker.click: " DOMAIN; fi
  DOMAIN="$(normalize_domain "$DOMAIN")"
  check_domain "$DOMAIN" || die "域名格式不正确：$DOMAIN"

  if [[ -z "$NODE_MODE" ]]; then
    echo
    blue "请选择本机角色："
    echo "  1) direct  最终落地"
    echo "  2) relay   链式中转"
    read -rp "请选择 [1/2]: " NODE_MODE
  fi
  NODE_MODE="$(normalize_node_mode "$NODE_MODE")" || die "NODE_MODE 只能是 direct 或 relay"

  if [[ -z "$SECURITY_MODE" ]]; then
    echo
    blue "请选择入口安全模式："
    echo "  1) TLS + Vision"
    echo "  2) REALITY + Vision（推荐作为大陆入口）"
    echo "  3) dual：两者同时启用（推荐，主备）"
    read -rp "请选择 [1/2/3，默认3]: " SECURITY_MODE
    [[ -n "$SECURITY_MODE" ]] || SECURITY_MODE=3
  fi
  SECURITY_MODE="$(normalize_security_mode "$SECURITY_MODE")" || die "SECURITY_MODE 只能是 tls/reality/dual"

  if [[ "$SECURITY_MODE" == tls ]]; then
    [[ -n "$TLS_PORT" ]] || TLS_PORT="${LISTEN_PORT:-443}"
  elif [[ "$SECURITY_MODE" == reality ]]; then
    [[ -n "$REALITY_PORT" ]] || REALITY_PORT="${LISTEN_PORT:-443}"
  else
    [[ -n "$REALITY_PORT" ]] || REALITY_PORT="${LISTEN_PORT:-443}"
    [[ -n "$TLS_PORT" ]] || TLS_PORT=8443
  fi

  if uses_tls; then check_port_number "$TLS_PORT" || die "TLS_PORT 格式不正确"; fi
  if uses_reality; then check_port_number "$REALITY_PORT" || die "REALITY_PORT 格式不正确"; fi
  if [[ "$SECURITY_MODE" == dual && "$TLS_PORT" == "$REALITY_PORT" ]]; then die "dual 模式下 TLS_PORT 与 REALITY_PORT 不能相同"; fi

  if uses_reality; then
    REALITY_TARGET_MODE="$(normalize_reality_target_mode "$REALITY_TARGET_MODE")" || die "REALITY_TARGET_MODE 只能是 local 或 remote"
    if [[ "$REALITY_TARGET_MODE" == local ]]; then
      REALITY_TARGET="xray-nginx:8443"
      REALITY_SERVER_NAME="$DOMAIN"
    else
      if [[ -z "$REALITY_TARGET" ]]; then
        echo
        yellow "REALITY remote 目标应由你明确指定；不要随手用 Cloudflare CDN 域名。"
        read -rp "REALITY 目标域名[:端口]，例如 www.example.com:443: " REALITY_TARGET
      fi
      split_host_port "$REALITY_TARGET"
      check_host "$SPLIT_HOST" || die "REALITY_TARGET 主机格式不正确"
      check_port_number "$SPLIT_PORT" || die "REALITY_TARGET 端口格式不正确"
      REALITY_TARGET="${SPLIT_HOST}:${SPLIT_PORT}"
      [[ -n "$REALITY_SERVER_NAME" ]] || REALITY_SERVER_NAME="$SPLIT_HOST"
    fi
  fi

  if [[ "$NODE_MODE" == relay ]]; then
    echo
    blue "配置下一跳 VLESS："
    if [[ -n "$UPSTREAM_URI" ]]; then
      parse_upstream_uri "$UPSTREAM_URI"
    elif [[ -z "$UPSTREAM_ADDRESS" || -z "$UPSTREAM_PORT" || -z "$UPSTREAM_UUID" ]]; then
      read -rp "请粘贴下一跳 VLESS 分享链接: " UPSTREAM_URI
      parse_upstream_uri "$UPSTREAM_URI"
    fi
    [[ -n "$UPSTREAM_SECURITY" ]] || UPSTREAM_SECURITY=tls
    validate_upstream
    green "链式出口：${UPSTREAM_ADDRESS}:${UPSTREAM_PORT} (${UPSTREAM_SECURITY})"
  fi

  if needs_cert; then
    if [[ -z "$EMAIL" ]]; then read -rp "请输入申请证书用邮箱: " EMAIL; fi
    [[ "$EMAIL" == *@* ]] || die "邮箱格式不正确"

    if [[ -z "$CF_API_TOKEN" ]]; then
      echo
      yellow "请粘贴 Cloudflare API Token（至少 Zone:DNS:Edit）。"
      read -rp "Cloudflare API Token: " CF_API_TOKEN
    fi
    CF_API_TOKEN="$(echo "$CF_API_TOKEN" | tr -d '[:space:]')"
    [[ -n "$CF_API_TOKEN" ]] || die "Cloudflare API Token 不能为空"
  fi

  if [[ -z "$UUID" ]]; then UUID="$(cat /proc/sys/kernel/random/uuid)"; fi
  [[ "$UUID" =~ ^[0-9a-fA-F-]{36}$ ]] || die "UUID 格式不正确：$UUID"
}

verify_cf_token() {
  needs_cert || return 0
  blue "校验 Cloudflare API Token..."
  local response status body
  response="$(curl -sS --connect-timeout 10 --max-time 20 \
    https://api.cloudflare.com/client/v4/user/tokens/verify \
    -H "Authorization: Bearer ${CF_API_TOKEN}" \
    -H 'Content-Type: application/json' \
    -w $'\nHTTP_STATUS:%{http_code}')" || die "Cloudflare Token 校验请求失败"
  status="$(echo "$response" | awk -F 'HTTP_STATUS:' 'NF>1{print $2}' | tail -n1)"
  body="$(echo "$response" | sed '/HTTP_STATUS:/d')"
  [[ "$status" == 200 ]] && echo "$body" | grep -q '"success"[[:space:]]*:[[:space:]]*true' || die "Cloudflare Token 校验失败：$body"
  green "Cloudflare Token 校验成功"
}

warn_dns() {
  local public_ip domain_ip
  public_ip="$(curl -4s --max-time 5 https://api.ipify.org || true)"
  domain_ip="$(getent ahostsv4 "$DOMAIN" 2>/dev/null | awk '{print $1; exit}' || true)"
  if [[ -n "$public_ip" && -n "$domain_ip" && "$public_ip" != "$domain_ip" ]]; then
    yellow "警告：当前 VPS IPv4 是 $public_ip，但 $DOMAIN 解析到 $domain_ip"
    yellow "VLESS RAW/TLS/REALITY 建议使用 DNS only 直连源站。"
  fi
}

prepare_dirs() {
  blue "准备目录：$APP_DIR"
  if [[ -f "$APP_DIR/docker-compose.yml" ]]; then
    yellow "检测到旧部署，先停止旧容器..."
    (cd "$APP_DIR" && compose down --remove-orphans) || true
  fi
  mkdir -p "$APP_DIR"/{xray,nginx,certbot/conf,site,certs}

  cat > "$APP_DIR/site/index.html" <<HTML
<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width,initial-scale=1">
  <title>${DOMAIN}</title>
  <style>
    body{font-family:system-ui,-apple-system,BlinkMacSystemFont,"Segoe UI",sans-serif;max-width:760px;margin:10vh auto;padding:0 24px;color:#222;line-height:1.6}
    h1{font-size:28px}.muted{color:#666}code{background:#f4f4f4;padding:2px 6px;border-radius:5px}
  </style>
</head>
<body>
  <h1>${DOMAIN}</h1>
  <p>Service is online.</p>
  <p class="muted">This host provides private application and API services.</p>
</body>
</html>
HTML

  cat > "$APP_DIR/cloudflare.ini" <<CF
dns_cloudflare_api_token = ${CF_API_TOKEN}
CF
  chmod 600 "$APP_DIR/cloudflare.ini"
}

check_ports() {
  blue "检查端口占用..."
  if uses_tls && port_in_use "$TLS_PORT"; then die "TLS_PORT ${TLS_PORT} 已被占用"; fi
  if uses_reality && port_in_use "$REALITY_PORT"; then die "REALITY_PORT ${REALITY_PORT} 已被占用"; fi
}

write_compose() {
  blue "写入 docker-compose.yml..."
  local port_lines=""
  if uses_tls; then port_lines+="      - \"${TLS_PORT}:${TLS_PORT}\""$'\n'; fi
  if uses_reality; then port_lines+="      - \"${REALITY_PORT}:${REALITY_PORT}\""$'\n'; fi

  cat > "$APP_DIR/docker-compose.yml" <<YAML
services:
  nginx:
    image: nginx:alpine
    container_name: xray-nginx
    restart: unless-stopped
    expose:
      - "8080"
      - "8443"
    volumes:
      - ./nginx/default.conf:/etc/nginx/conf.d/default.conf:ro
      - ./site:/usr/share/nginx/html:ro
      - ./certs:/etc/nginx/certs:ro
    networks:
      - xray-net

  xray:
    image: ghcr.io/xtls/xray-core:latest
    container_name: xray-core
    restart: unless-stopped
    depends_on:
      - nginx
    ports:
${port_lines}    volumes:
      - ./xray/config.json:/etc/xray/config.json:ro
      - ./certs:/etc/xray/certs:ro
    command: ["run", "-config", "/etc/xray/config.json"]
    networks:
      - xray-net

  certbot:
    image: certbot/dns-cloudflare
    container_name: xray-certbot
    volumes:
      - ./certbot/conf:/etc/letsencrypt
      - ./cloudflare.ini:/cloudflare.ini:ro

networks:
  xray-net:
    driver: bridge
YAML
}

write_nginx_config() {
  blue "写入本地伪装站配置..."
  cat > "$APP_DIR/nginx/default.conf" <<NGINX
server {
    listen 8080 default_server;
    server_name _;
    root /usr/share/nginx/html;
    index index.html;
    location / { try_files \$uri \$uri/ /index.html; }
}
NGINX

  if needs_cert; then
    cat >> "$APP_DIR/nginx/default.conf" <<NGINX

server {
    listen 8443 ssl default_server;
    server_name ${DOMAIN};
    ssl_certificate /etc/nginx/certs/fullchain.pem;
    ssl_certificate_key /etc/nginx/certs/privkey.pem;
    ssl_protocols TLSv1.2 TLSv1.3;
    root /usr/share/nginx/html;
    index index.html;
    location / { try_files \$uri \$uri/ /index.html; }
}
NGINX
  fi
}

pull_images() {
  blue "拉取 Xray/Nginx 镜像..."
  cd "$APP_DIR"
  compose pull nginx xray
  if needs_cert; then compose pull certbot; fi
}

obtain_cert() {
  needs_cert || return 0
  blue "通过 Cloudflare DNS-01 申请/续用证书..."
  cd "$APP_DIR"
  compose run --rm certbot certonly \
    --dns-cloudflare \
    --dns-cloudflare-credentials /cloudflare.ini \
    --dns-cloudflare-propagation-seconds 60 \
    -d "$DOMAIN" \
    --email "$EMAIL" \
    --agree-tos \
    --no-eff-email \
    --non-interactive \
    --keep-until-expiring
  [[ -f "$APP_DIR/certbot/conf/live/$DOMAIN/fullchain.pem" ]] || die "证书申请失败"
}

sync_cert() {
  needs_cert || return 0
  cp -L "$APP_DIR/certbot/conf/live/$DOMAIN/fullchain.pem" "$APP_DIR/certs/fullchain.pem"
  cp -L "$APP_DIR/certbot/conf/live/$DOMAIN/privkey.pem" "$APP_DIR/certs/privkey.pem"
  chmod 644 "$APP_DIR/certs/fullchain.pem" "$APP_DIR/certs/privkey.pem"
}

generate_reality_keys() {
  uses_reality || return 0
  blue "准备 REALITY X25519 密钥..."
  cd "$APP_DIR"
  local out

  if [[ -n "$REALITY_PRIVATE_KEY" && -z "$REALITY_PUBLIC_KEY" ]]; then
    out="$(compose run --rm --no-deps xray x25519 -i "$REALITY_PRIVATE_KEY")"
    REALITY_PUBLIC_KEY="$(echo "$out" | awk -F': *' '/Password/{print $2; exit}')"
  elif [[ -z "$REALITY_PRIVATE_KEY" ]]; then
    out="$(compose run --rm --no-deps xray x25519)"
    REALITY_PRIVATE_KEY="$(echo "$out" | awk -F': *' '/PrivateKey|Private key/{print $2; exit}')"
    REALITY_PUBLIC_KEY="$(echo "$out" | awk -F': *' '/Password/{print $2; exit}')"
  fi

  [[ -n "$REALITY_PRIVATE_KEY" && -n "$REALITY_PUBLIC_KEY" ]] || die "无法生成 REALITY X25519 密钥"
  if [[ -z "$REALITY_SHORT_ID" ]]; then REALITY_SHORT_ID="$(openssl rand -hex 8)"; fi
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

build_upstream_outbound() {
  if [[ "$NODE_MODE" != relay ]]; then
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
    local pqv=""
    if [[ -n "$UPSTREAM_REALITY_MLDSA65_VERIFY" ]]; then
      pqv=",\n          \"mldsa65Verify\": \"${UPSTREAM_REALITY_MLDSA65_VERIFY}\""
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
          "shortId": "${UPSTREAM_REALITY_SHORT_ID}"${pqv}
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
  local route_outbound=direct inbounds="" outbounds
  [[ "$NODE_MODE" == relay ]] && route_outbound=upstream-vless

  if uses_tls; then
    inbounds+=$(cat <<JSON
    {
      "tag": "vless-tls-in",
      "listen": "0.0.0.0",
      "port": ${TLS_PORT},
      "protocol": "vless",
      "settings": {
        "clients": [{"id": "${UUID}", "flow": "xtls-rprx-vision"}],
        "decryption": "none",
        "fallbacks": [{"dest": "xray-nginx:8080"}]
      },
      "streamSettings": {
        "method": "raw",
        "security": "tls",
        "tlsSettings": {
          "minVersion": "1.2",
          "certificates": [{
            "certificateFile": "/etc/xray/certs/fullchain.pem",
            "keyFile": "/etc/xray/certs/privkey.pem"
          }]
        }
      }
    }
JSON
)
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
        "clients": [{"id": "${UUID}", "flow": "xtls-rprx-vision"}],
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
          "shortIds": ["${REALITY_SHORT_ID}"]
        }
      }
    }
JSON
)
  fi

  outbounds="$(build_upstream_outbound)"

  cat > "$APP_DIR/xray/config.json" <<JSON
{
  "log": {"loglevel": "warning"},
  "routing": {
    "domainStrategy": "AsIs",
    "rules": [
      {
        "type": "field",
        "inboundTag": ["vless-tls-in", "vless-reality-in"],
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
  chmod 600 "$APP_DIR/xray/config.json"
}

validate_xray_config() {
  blue "校验 Xray 配置..."
  cd "$APP_DIR"
  compose run --rm --no-deps xray run -test -config /etc/xray/config.json >/dev/null
  green "Xray 配置校验通过"
}

check_upstream_reachability() {
  [[ "$NODE_MODE" == relay ]] || return 0
  blue "检查下一跳 TCP：${UPSTREAM_ADDRESS}:${UPSTREAM_PORT}"
  if timeout 6 bash -c "</dev/tcp/${UPSTREAM_ADDRESS}/${UPSTREAM_PORT}" 2>/dev/null; then
    green "下一跳端口可达"
  else
    yellow "警告：当前无法连接下一跳；配置仍保留。"
  fi
}

start_services() {
  blue "启动 Nginx 和 Xray..."
  cd "$APP_DIR"
  compose up -d nginx xray
  sleep 3
  docker ps --format '{{.Names}}' | grep -q '^xray-core$' || { docker logs xray-core || true; die "Xray 容器启动失败"; }
  green "Xray 启动成功"
}

write_client_info() {
  local mode_text tls_uri reality_uri
  if [[ "$NODE_MODE" == relay ]]; then
    mode_text="relay（下一跳 ${UPSTREAM_SECURITY}://${UPSTREAM_ADDRESS}:${UPSTREAM_PORT}）"
  else
    mode_text="direct（最终落地）"
  fi

  : > "$APP_DIR/client.txt"
  {
    echo "节点模式: ${mode_text}"
    echo "域名: ${DOMAIN}"
    echo "UUID: ${UUID}"
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
      echo "[备用] VLESS + TLS + Vision"
      echo "端口: ${TLS_PORT}"
      echo "SNI: ${DOMAIN}"
      echo "分享链接:"
      echo "$tls_uri"
      echo
    fi
  } >> "$APP_DIR/client.txt"
  chmod 600 "$APP_DIR/client.txt"
}

setup_renew() {
  needs_cert || return 0
  blue "配置证书自动续期..."
  cat > "$APP_DIR/renew.sh" <<RENEW
#!/usr/bin/env bash
set -Eeuo pipefail
APP_DIR="$APP_DIR"
DOMAIN="$DOMAIN"
cd "\$APP_DIR"
docker compose run --rm certbot renew --quiet
cp -L "\$APP_DIR/certbot/conf/live/\$DOMAIN/fullchain.pem" "\$APP_DIR/certs/fullchain.pem"
cp -L "\$APP_DIR/certbot/conf/live/\$DOMAIN/privkey.pem" "\$APP_DIR/certs/privkey.pem"
chmod 644 "\$APP_DIR/certs/fullchain.pem" "\$APP_DIR/certs/privkey.pem"
docker compose restart nginx xray >/dev/null 2>&1
RENEW
  chmod +x "$APP_DIR/renew.sh"
  systemctl enable --now cron >/dev/null 2>&1 || true
  crontab -l 2>/dev/null | grep -v "$APP_DIR/renew.sh" > /tmp/xray-cron-old || true
  echo "23 4 * * * $APP_DIR/renew.sh >/dev/null 2>&1" >> /tmp/xray-cron-old
  crontab /tmp/xray-cron-old
  rm -f /tmp/xray-cron-old
}

show_result() {
  green "安装完成"
  echo
  blue "配置目录：$APP_DIR"
  blue "客户端信息：$APP_DIR/client.txt"
  blue "入口模式：$SECURITY_MODE"
  blue "节点角色：$NODE_MODE"
  echo
  cat "$APP_DIR/client.txt"
  echo
  yellow "说明："
  if uses_reality; then
    yellow "- REALITY + Vision 建议作为大陆客户端的主入口。"
    yellow "- REALITY target: ${REALITY_TARGET}，serverName: ${REALITY_SERVER_NAME}"
    if [[ "$REALITY_TARGET_MODE" == local ]]; then
      if [[ "$REALITY_PORT" == 443 ]]; then
        yellow "- 普通浏览器访问 https://${DOMAIN} 会落到本地 HTTPS 伪装站。"
      else
        yellow "- 普通浏览器访问 https://${DOMAIN}:${REALITY_PORT} 会落到本地 HTTPS 伪装站。"
      fi
    fi
  fi
  if uses_tls; then
    yellow "- TLS Vision 入口也带本地 Nginx fallback，可作为备用。"
  fi
  yellow "- Cloudflare DNS 请使用 DNS only，不要给 RAW VLESS 开橙云。"
  yellow "- REALITY/TLS 都不是绝对无法识别或封锁，只是降低特征并改善正常 TLS 外观。"
}

main() {
  install_base_tools
  install_docker_if_needed
  ask_inputs
  verify_cf_token
  warn_dns
  prepare_dirs
  check_ports
  write_compose
  write_nginx_config
  pull_images
  obtain_cert
  sync_cert
  generate_reality_keys
  write_xray_config
  validate_xray_config
  check_upstream_reachability
  start_services
  write_client_info
  setup_renew
  show_result
}

main "$@"

