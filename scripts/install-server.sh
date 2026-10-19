#!/bin/sh
# 可从源码、发行包或 curl 管道启动；兼容 Alpine 的 /bin/sh 与 macOS。
set -eu
PATH=/opt/homebrew/bin:/opt/homebrew/sbin:/usr/local/bin:/usr/local/sbin:/usr/bin:/bin:/usr/sbin:/sbin:$PATH
export PATH
script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
case "${1:-}" in
  help|--help|-h)
    if [ ! -f "$script_dir/chain_install.py" ]; then
      echo 'Chain Panel：Linux（systemd/OpenRC）与 macOS，x86_64 / ARM64。'
      echo '直接运行安装；安装后输入 chainctl 打开管理菜单。'
      exit 0
    fi ;;
esac
case "$(uname -s)" in
  Linux|Darwin) ;;
  *) echo '仅支持 Linux 和 macOS。' >&2; exit 1 ;;
esac
case "$(uname -m)" in
  x86_64|aarch64|arm64) ;;
  *) echo '仅支持 x86_64 / ARM64。' >&2; exit 1 ;;
esac
find_python() {
  for candidate in python3 python3.14 python3.13 python3.12 python3.11 python3.10; do
    if command -v "$candidate" >/dev/null 2>&1 && "$candidate" -c 'import sys; sys.exit(sys.version_info < (3, 10))' 2>/dev/null; then
      python=$(command -v "$candidate")
      return 0
    fi
  done
  return 1
}
if ! find_python; then
  if [ "$(id -u)" != 0 ]; then
    echo '安装运行依赖需要管理员权限，请使用 root 或 sudo 运行安装脚本。' >&2
    exit 1
  fi
  echo '正在安装 Python 与 HTTPS 证书…'
  if [ "$(uname -s)" = Darwin ]; then
    if ! command -v brew >/dev/null 2>&1 || [ -z "${SUDO_USER:-}" ] || [ "$SUDO_USER" = root ]; then
      echo '请先安装 Homebrew（https://brew.sh），再从普通管理员账户通过 sudo 运行本脚本。' >&2
      exit 1
    fi
    sudo -H -u "$SUDO_USER" "$(command -v brew)" install python ca-certificates
  elif command -v apt-get >/dev/null 2>&1; then
    apt-get update
    apt-get install -y python3 ca-certificates
  elif command -v apk >/dev/null 2>&1; then
    apk add --no-cache python3 ca-certificates
  elif command -v dnf >/dev/null 2>&1; then
    dnf install -y python3.11 ca-certificates || dnf install -y python3 ca-certificates
  elif command -v yum >/dev/null 2>&1; then
    yum install -y python3 ca-certificates
  elif command -v pacman >/dev/null 2>&1; then
    pacman -S --needed --noconfirm python ca-certificates
  elif command -v zypper >/dev/null 2>&1; then
    zypper --non-interactive install python311 ca-certificates
  else
    echo '请先安装 Python 3.10+ 与 CA 证书，再运行本脚本。' >&2
    exit 1
  fi
  find_python || { echo '系统源未提供 Python 3.10+，请先安装兼容版本；不会替换系统 Python。' >&2; exit 1; }
fi
export PYTHONDONTWRITEBYTECODE=1
bootstrap_dir=''
cleanup() { if [ -n "$bootstrap_dir" ]; then rm -rf -- "$bootstrap_dir"; fi; }
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
if [ ! -f "$script_dir/chain_install.py" ] || [ ! -f "$script_dir/chain_common.py" ] || [ ! -f "$script_dir/chain_network.py" ] || [ ! -f "$script_dir/chain_ingress.py" ] || [ ! -f "$script_dir/chain_platform.py" ] || [ ! -f "$script_dir/chain_legacy.py" ] || [ ! -f "$script_dir/chain_time.py" ]; then
  bootstrap_dir=$(mktemp -d "${TMPDIR:-/tmp}/chain-bootstrap-XXXXXXXX")
  "$python" -B - "$bootstrap_dir" <<'PY'
import json, pathlib, re, sys, urllib.request
root = pathlib.Path(sys.argv[1])
repo = 'dovetaill/vless-chain'
request = urllib.request.Request(f'https://api.github.com/repos/{repo}/commits/main', headers={'User-Agent': 'chain-installer'})
with urllib.request.urlopen(request, timeout=30) as response:
    commit = json.load(response)['sha']
if not re.fullmatch('[0-9a-f]{40}', commit):
    raise SystemExit('安装器版本信息无效。')
for name in ['chain_common.py', 'chain_install.py', 'chain_network.py', 'chain_ingress.py', 'chain_platform.py', 'chain_legacy.py', 'chain_time.py']:
    with urllib.request.urlopen(f'https://raw.githubusercontent.com/{repo}/{commit}/scripts/{name}', timeout=30) as response:
        value = response.read(1024 * 1024 + 1)
    if len(value) > 1024 * 1024:
        raise SystemExit('安装器下载大小超限。')
    (root / name).write_bytes(value)
PY
  script_dir=$bootstrap_dir
fi
if [ "$#" -eq 0 ]; then set -- setup; fi
if [ ! -t 0 ] && [ -r /dev/tty ] && { true </dev/tty; } 2>/dev/null; then
  "$python" -B "$script_dir/chain_install.py" "$@" </dev/tty
else
  "$python" -B "$script_dir/chain_install.py" "$@"
fi
