#!/usr/bin/env bash
# 可从源码、发行包或 curl 管道启动；交互输入始终来自终端。
set -Eeuo pipefail
script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]:-./install-server.sh}")" && pwd)
if [[ ! -f "$script_dir/chain_install.py" && ( "${1:-}" == help || "${1:-}" == --help || "${1:-}" == -h ) ]]; then
  echo 'Chain Panel 安装向导：直接运行本脚本，按提示安装控制台或接入节点。'
  echo '安装后输入 chainctl 打开管理菜单，chainctl help 查看命令。'
  exit 0
fi
if ! command -v python3 >/dev/null || ! python3 -c 'import sys; sys.exit(sys.version_info < (3, 10))'; then
  if (( EUID != 0 )); then
    echo '安装运行依赖需要管理员权限，请使用 sudo 运行安装脚本。' >&2
    exit 1
  fi
  echo '正在安装 Python 与 HTTPS 证书…'
  if command -v apt-get >/dev/null; then
    apt-get update
    apt-get install -y python3 ca-certificates
  elif command -v dnf >/dev/null; then
    dnf install -y python3 ca-certificates
  else
    echo '请先安装 Python 3.10+，再运行本脚本。' >&2
    exit 1
  fi
  python3 -c 'import sys; sys.exit(sys.version_info < (3, 10))' || { echo '系统 Python 版本过旧，需要 Python 3.10+。' >&2; exit 1; }
fi
export PYTHONDONTWRITEBYTECODE=1
bootstrap_dir=''
cleanup() { if [[ -n "$bootstrap_dir" ]]; then rm -rf -- "$bootstrap_dir"; fi; }
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
if [[ ! -f "$script_dir/chain_install.py" || ! -f "$script_dir/chain_common.py" ]]; then
  bootstrap_dir=$(mktemp -d -t chain-bootstrap-XXXXXXXX)
  python3 -B - "$bootstrap_dir" <<'PY'
import json, pathlib, re, sys, urllib.request
root = pathlib.Path(sys.argv[1])
repo = 'dovetaill/vless-chain'
request = urllib.request.Request(f'https://api.github.com/repos/{repo}/commits/main', headers={'User-Agent': 'chain-installer'})
with urllib.request.urlopen(request, timeout=30) as response:
    commit = json.load(response)['sha']
if not re.fullmatch('[0-9a-f]{40}', commit):
    raise SystemExit('安装器版本信息无效。')
for name in ['chain_common.py', 'chain_install.py']:
    with urllib.request.urlopen(f'https://raw.githubusercontent.com/{repo}/{commit}/scripts/{name}', timeout=30) as response:
        value = response.read(1024 * 1024 + 1)
    if len(value) > 1024 * 1024:
        raise SystemExit('安装器下载大小超限。')
    (root / name).write_bytes(value)
PY
  script_dir=$bootstrap_dir
fi
if [[ ! -t 0 && -r /dev/tty ]] && { true </dev/tty; } 2>/dev/null; then
  python3 -B "$script_dir/chain_install.py" "$@" </dev/tty
else
  python3 -B "$script_dir/chain_install.py" "$@"
fi
