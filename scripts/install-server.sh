#!/usr/bin/env bash
# 源码仓库、解压后的发行包均可使用；同目录的 Python 文件需一同保留。
set -Eeuo pipefail
script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
command -v python3 >/dev/null || { echo '需要 Python 3.10+，Debian/Ubuntu 可执行 apt-get install python3。' >&2; exit 1; }
export PYTHONDONTWRITEBYTECODE=1
exec python3 -B "$script_dir/chain_install.py" "$@"
