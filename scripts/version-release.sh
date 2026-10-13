#!/usr/bin/env bash
# 从任意工作目录启动版本、构建、发布及运维菜单。
set -Eeuo pipefail
script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
command -v python3 >/dev/null || { echo '需要 Python 3.10+。' >&2; exit 1; }
export PYTHONDONTWRITEBYTECODE=1
exec python3 -B "$script_dir/chain_release.py" "$@"
