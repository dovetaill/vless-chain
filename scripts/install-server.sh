#!/bin/sh
# Bootstrap only: download the verified native chainctl, then hand installation to Rust.
set -eu
PATH=/opt/homebrew/bin:/opt/homebrew/sbin:/usr/local/bin:/usr/local/sbin:/usr/bin:/bin:/usr/sbin:/sbin:$PATH
export PATH
case "${1:-}" in help|--help|-h)
  echo 'Chain Panel 原生安装：Linux x86_64 / ARM64、macOS ARM64。'
  echo '安装后运行 chainctl；自动化安装使用 --non-interactive。'; exit 0;; esac
case "$(uname -s)" in Linux) platform=linux;; Darwin) platform=darwin;; *) echo '不支持的操作系统' >&2; exit 1;; esac
case "$(uname -m)" in x86_64) arch=x86_64;; aarch64|arm64) arch=aarch64;; *) echo '不支持的 CPU 架构' >&2; exit 1;; esac
[ "$platform-$arch" != darwin-x86_64 ] || { echo 'macOS 仅支持 ARM64' >&2; exit 1; }
script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
if [ -x "$script_dir/../bin/chainctl" ]; then
  case "${1:-}" in ''|-*) set -- setup "$@";; esac
  exec "$script_dir/../bin/chainctl" "$@"
fi
command -v curl >/dev/null || { echo '请先安装 curl 和 CA 证书' >&2; exit 1; }
command -v tar >/dev/null || { echo '请先安装 tar' >&2; exit 1; }
bootstrap_dir=$(mktemp -d "${TMPDIR:-/tmp}/chain-bootstrap-XXXXXXXX")
bootstrap_pid=
cleanup() {
  if [ -n "$bootstrap_pid" ]; then
    kill -TERM "$bootstrap_pid" 2>/dev/null || true
    wait "$bootstrap_pid" 2>/dev/null || true
  fi
  rm -rf -- "$bootstrap_dir"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
repo=https://github.com/dovetaill/vless-chain
latest=$(curl --proto '=https' --proto-redir '=https' -fsSL --max-time 30 -o /dev/null -w '%{url_effective}' "$repo/releases/latest")
tag=${latest##*/}
version=${tag#v}
case "$tag" in v[0-9]*.[0-9]*.[0-9]*) ;; *) echo '正式版本信息无效' >&2; exit 1;; esac
case "$version" in *[!0-9.]*|*..*|.*|*.) echo '版本号无效' >&2; exit 1;; esac
[ "$(printf '%s' "$version" | tr -cd '.' | wc -c | tr -d ' ')" = 2 ] || exit 1
archive=chain-panel-$version-$platform-$arch.tar.gz
for name in SHA256SUMS "$archive"; do
  curl --proto '=https' --proto-redir '=https' -fSL --max-time 180 --max-filesize 536870912 "$repo/releases/download/$tag/$name" -o "$bootstrap_dir/$name"
done
expected=$(awk -v name="$archive" '$2 == name { print $1; count++ } END { if (count != 1) exit 1 }' "$bootstrap_dir/SHA256SUMS")
case "$expected" in *[!0-9a-f]*|'') echo 'SHA256 清单无效' >&2; exit 1;; esac
[ "${#expected}" = 64 ] || exit 1
if command -v sha256sum >/dev/null; then
  actual=$(sha256sum "$bootstrap_dir/$archive"); actual=${actual%% *}
else
  actual=$(shasum -a 256 "$bootstrap_dir/$archive"); actual=${actual%% *}
fi
[ "$actual" = "$expected" ] || { echo '发行包校验失败' >&2; exit 1; }
# Extract one file as bytes, never apply archive paths, symlinks or ownership on the host.
tar -xOzf "$bootstrap_dir/$archive" bin/chainctl > "$bootstrap_dir/chainctl" || {
  echo '此正式版本尚未提供原生 chainctl，请等待包含 Rust 接管的发行版。' >&2; exit 1;
}
[ -s "$bootstrap_dir/chainctl" ] || exit 1
chmod 700 "$bootstrap_dir/chainctl"
case "${1:-}" in ''|-*) set -- setup "$@";; esac
has_package=false
for arg in "$@"; do [ "$arg" != --package ] || has_package=true; done
if [ "$has_package" = false ]; then set -- "$@" --package "$bootstrap_dir/$archive"; fi
if [ ! -t 0 ] && [ -r /dev/tty ] && { true </dev/tty; } 2>/dev/null; then
  "$bootstrap_dir/chainctl" "$@" </dev/tty &
else
  "$bootstrap_dir/chainctl" "$@" <&0 &
fi
bootstrap_pid=$!
bootstrap_result=0
wait "$bootstrap_pid" || bootstrap_result=$?
bootstrap_pid=
exit "$bootstrap_result"
