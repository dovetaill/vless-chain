#!/bin/sh
# Bootstrap only: download the verified native chainctl, then hand installation to Rust.
set -eu
PATH=/opt/homebrew/bin:/opt/homebrew/sbin:/usr/local/bin:/usr/local/sbin:/usr/bin:/bin:/usr/sbin:/sbin:$PATH
export PATH
case "${1:-}" in help|--help|-h)
  echo 'Chain Panel 原生安装：Linux x86_64 / ARM64、macOS ARM64。'
  echo '安装后运行 chainctl；自动化安装使用 --non-interactive。'
  echo '指定本地发行包：update --package /path/package.tar.gz --non-interactive（同目录需有 SHA256SUMS）。'
  echo '也可使用 --package https://下载站/目录/chain-panel-VERSION-linux-x86_64.tar.gz，自动下载同目录的 SHA256SUMS。'; exit 0;; esac
case "$(uname -s)" in Linux) platform=linux;; Darwin) platform=darwin;; *) echo '不支持的操作系统' >&2; exit 1;; esac
case "$(uname -m)" in x86_64) arch=x86_64;; aarch64|arm64) arch=aarch64;; *) echo '不支持的 CPU 架构' >&2; exit 1;; esac
[ "$platform-$arch" != darwin-x86_64 ] || { echo 'macOS 仅支持 ARM64' >&2; exit 1; }
local_package=
expect_package=false
for arg in "$@"; do
  if [ "$expect_package" = true ]; then
    [ -n "$arg" ] && [ -z "$local_package" ] || { echo '--package 路径为空或重复' >&2; exit 1; }
    local_package=$arg
    expect_package=false
  elif [ "$arg" = --package ]; then
    [ -z "$local_package" ] || { echo '--package 不可重复' >&2; exit 1; }
    expect_package=true
  fi
done
[ "$expect_package" = false ] || { echo '--package 缺少路径' >&2; exit 1; }
script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
if [ -z "$local_package" ] && [ -x "$script_dir/../bin/chainctl" ]; then
  if [ "${1:-}" = bootstrap-native ]; then
    [ "$#" = 2 ] && [ ! -e "$2" ] && [ ! -L "$2" ] || { echo '原生工具输出路径必须不存在' >&2; exit 1; }
    cp "$script_dir/../bin/chainctl" "$2"
    chmod 700 "$2"
    exit 0
  fi
  case "${1:-}" in ''|-*) set -- setup "$@";; esac
  exec "$script_dir/../bin/chainctl" "$@"
fi
package_url=
case "$local_package" in
  https://*) package_url=$local_package;;
  *://*) echo '下载地址必须使用 HTTPS' >&2; exit 1;;
esac
if [ -z "$local_package" ] || [ -n "$package_url" ]; then
  command -v curl >/dev/null || { echo '请先安装 curl 和 CA 证书' >&2; exit 1; }
fi
command -v tar >/dev/null || { echo '请先安装 tar' >&2; exit 1; }
bootstrap_dir=
for bootstrap_parent in "${TMPDIR:-/tmp}" /var/tmp; do
  [ -d "$bootstrap_parent" ] && [ -w "$bootstrap_parent" ] || continue
  bootstrap_space=$(LC_ALL=C df -Pk "$bootstrap_parent" 2>/dev/null) || continue
  # Budget for the 512 MiB archive, 1 GiB stage, bootstrap binary and headroom.
  printf '%s\n' "$bootstrap_space" | awk 'NR == 2 && $4 ~ /^[0-9]+$/ && $4 >= 3145728 { enough=1 } END { exit !enough }' || continue
  bootstrap_dir=$(mktemp -d "$bootstrap_parent/chain-bootstrap-XXXXXXXX") && break
done
[ -n "$bootstrap_dir" ] || {
  printf '未找到可用的安装临时目录（已检查 %s、/var/tmp；需至少 3 GiB 可用空间），请释放空间或将 TMPDIR 指向其他磁盘目录。\n' "${TMPDIR:-/tmp}" >&2
  exit 1
}
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
if [ "$bootstrap_parent" != "${TMPDIR:-/tmp}" ]; then
  printf '原临时目录空间不足或不可用，已改用 %s；安装结束后自动清理。\n' "$bootstrap_parent" >&2
fi
TMPDIR=$bootstrap_dir
export TMPDIR
if [ -n "$package_url" ]; then
  archive=${package_url##*/}
  case "$archive" in chain-panel-*-$platform-$arch.tar.gz) ;; *) echo '发行包名称或平台架构不匹配' >&2; exit 1;; esac
  for name in SHA256SUMS "$archive"; do
    curl --proto '=https' --proto-redir '=https' -fSL --max-time 180 --max-filesize 536870912 "${package_url%/*}/$name" -o "$bootstrap_dir/$name"
  done
  local_package=$bootstrap_dir/$archive
  # Replace only the package argument; chainctl receives the verified local download.
  remaining=$#
  expect_package=false
  while [ "$remaining" -gt 0 ]; do
    arg=$1; shift
    if [ "$expect_package" = true ]; then arg=$local_package; expect_package=false
    elif [ "$arg" = --package ]; then expect_package=true
    fi
    set -- "$@" "$arg"
    remaining=$((remaining - 1))
  done
elif [ -n "$local_package" ]; then
  [ -f "$local_package" ] || { echo '本地发行包不存在' >&2; exit 1; }
  package_parent=$(CDPATH= cd -- "$(dirname -- "$local_package")" && pwd)
  archive=$(basename -- "$local_package")
  [ -f "$package_parent/SHA256SUMS" ] || { echo '请将 SHA256SUMS 与发行包放在同一目录' >&2; exit 1; }
  cp "$local_package" "$bootstrap_dir/$archive"
  cp "$package_parent/SHA256SUMS" "$bootstrap_dir/SHA256SUMS"
else
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
fi
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
if [ "${1:-}" = bootstrap-native ]; then
  [ "$#" = 2 ] && [ ! -e "$2" ] && [ ! -L "$2" ] || { echo '原生工具输出路径必须不存在' >&2; exit 1; }
  cp "$bootstrap_dir/chainctl" "$2"
  chmod 700 "$2"
  exit 0
fi
case "${1:-}" in ''|-*) set -- setup "$@";; esac
if [ -z "$local_package" ]; then set -- "$@" --package "$bootstrap_dir/$archive"; fi
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
