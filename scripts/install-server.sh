#!/bin/sh
# Bootstrap only: download the verified native chainctl, then hand installation to Rust.
set -eu
PATH=/opt/homebrew/bin:/opt/homebrew/sbin:/usr/local/bin:/usr/local/sbin:/usr/bin:/bin:/usr/sbin:/sbin:$PATH
export PATH
case "${1:-}" in help|--help|-h)
  echo 'Chain Panel 原生安装：Linux x86_64 / ARM64、macOS ARM64。'
  echo '安装后运行 chainctl；自动化安装使用 --non-interactive。'
  echo '面板安装必须明确填写域名并使用 HTTPS 443；自动化安装增加 --domain 域名。'
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
bootstrap_pid=
cleanup() {
  if [ -n "$bootstrap_pid" ]; then
    kill -TERM "$bootstrap_pid" 2>/dev/null || true
    wait "$bootstrap_pid" 2>/dev/null || true
  fi
  [ -z "$bootstrap_dir" ] || rm -rf -- "$bootstrap_dir"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

free_kib() {
  LC_ALL=C df -Pk "$1" 2>/dev/null | awk 'NR == 2 && $4 ~ /^[0-9]+$/ {print $4; found=1} END {exit !found}'
}
requested_package=$local_package
archive=
url_base=
if [ -n "$package_url" ]; then
  url_path=${package_url%%[?#]*}
  archive=${url_path##*/}
  url_base=${url_path%/*}
  case "$archive" in
    ''|.|..|*/*|-*) echo '发行包名称无效' >&2; exit 1;;
    *.tar.gz) ;;
    *) echo '发行包必须为 .tar.gz 压缩包' >&2; exit 1;;
  esac
elif [ -n "$local_package" ]; then
  [ -f "$local_package" ] || { echo '本地发行包不存在' >&2; exit 1; }
  package_parent=$(CDPATH= cd -- "$(dirname -- "$local_package")" && pwd)
  archive=$(basename -- "$local_package")
  [ -f "$package_parent/SHA256SUMS" ] || { echo '请将 SHA256SUMS 与发行包放在同一目录' >&2; exit 1; }
fi
fallback_parent=${CHAIN_BOOTSTRAP_FALLBACK:-/var/tmp}
for bootstrap_parent in "${TMPDIR:-/tmp}" "$fallback_parent"; do
  [ -d "$bootstrap_parent" ] && [ -w "$bootstrap_parent" ] || continue
  available=$(free_kib "$bootstrap_parent") || continue
  # Leave room for the checksum, filesystem metadata and error reporting.
  [ "$available" -gt 1024 ] || continue
  limit=$(((available - 1024) * 1024))
  [ "$limit" -le 536870912 ] || limit=536870912
  bootstrap_dir=$(mktemp -d "$bootstrap_parent/chain-bootstrap-XXXXXXXX") || continue
  if [ -z "$archive" ]; then
    repo=https://github.com/dovetaill/vless-chain
    latest=$(curl --proto '=https' --proto-redir '=https' -fsSL --max-time 30 -o /dev/null -w '%{url_effective}' "$repo/releases/latest")
    tag=${latest##*/}; version=${tag#v}
    case "$tag" in v[0-9]*.[0-9]*.[0-9]*) ;; *) echo '正式版本信息无效' >&2; exit 1;; esac
    case "$version" in *[!0-9.]*|*..*|.*|*.) echo '版本号无效' >&2; exit 1;; esac
    [ "$(printf '%s' "$version" | tr -cd '.' | wc -c | tr -d ' ')" = 2 ] || { echo '版本号无效' >&2; exit 1; }
    archive=chain-panel-$version-$platform-$arch.tar.gz
    url_base=$repo/releases/download/$tag
    package_url=$url_base/$archive
  fi
  if [ -n "$package_url" ]; then
    curl --proto '=https' --proto-redir '=https' -fSL --max-time 30 --max-filesize 65536 "$url_base/SHA256SUMS" -o "$bootstrap_dir/SHA256SUMS"
    if curl --proto '=https' --proto-redir '=https' -fSL --max-time 180 --max-filesize "$limit" "$package_url" -o "$bootstrap_dir/$archive"; then :
    else
      code=$?
      # curl 63 is its size limit, 23 is a local write failure.
      case "$code" in 23|63) rm -rf -- "$bootstrap_dir"; bootstrap_dir=; continue;; *) exit "$code";; esac
    fi
  else
    bytes=$(wc -c < "$local_package")
    if [ "$bytes" -gt "$limit" ]; then rm -rf -- "$bootstrap_dir"; bootstrap_dir=; continue; fi
    cp "$local_package" "$bootstrap_dir/$archive"
    cp "$package_parent/SHA256SUMS" "$bootstrap_dir/SHA256SUMS"
  fi
  expected=$(awk -v name="$archive" '$2 == name { print $1; count++ } END { if (count != 1) exit 1 }' "$bootstrap_dir/SHA256SUMS")
  case "$expected" in *[!0-9a-f]*|'') echo 'SHA256 清单无效' >&2; exit 1;; esac
  [ "${#expected}" = 64 ] || { echo 'SHA256 清单无效' >&2; exit 1; }
  if command -v sha256sum >/dev/null; then actual=$(sha256sum "$bootstrap_dir/$archive")
  else actual=$(shasum -a 256 "$bootstrap_dir/$archive"); fi
  [ "${actual%% *}" = "$expected" ] || { echo '发行包校验失败' >&2; exit 1; }
  # Only the verified chainctl member is expanded here. Native installation stages on the release filesystem.
  if ! tar -xOzf "$bootstrap_dir/$archive" bin/chainctl > "$bootstrap_dir/chainctl" 2>/dev/null; then
    available=$(free_kib "$bootstrap_parent") || available=0
    if [ "$available" -lt 1024 ]; then rm -rf -- "$bootstrap_dir"; bootstrap_dir=; continue; fi
    echo '发行包缺少原生 chainctl 或提取失败' >&2
    exit 1
  fi
  bytes=$(wc -c < "$bootstrap_dir/chainctl")
  [ "$bytes" -gt 0 ] || { echo '发行包缺少原生 chainctl' >&2; exit 1; }
  available=$(free_kib "$bootstrap_parent") || available=0
  if [ "$available" -lt 1024 ]; then
    rm -rf -- "$bootstrap_dir"; bootstrap_dir=; continue
  fi
  chmod 700 "$bootstrap_dir/chainctl"
  break
done
[ -n "$bootstrap_dir" ] || {
  printf '安装临时空间不足或不可用（已检查 %s、%s）；需要容纳本次压缩包和 chainctl。\n' "${TMPDIR:-/tmp}" "$fallback_parent" >&2
  exit 1
}
if [ "$bootstrap_parent" != "${TMPDIR:-/tmp}" ]; then
  printf '原临时目录空间不足或不可用，已改用 %s；安装结束后自动清理。\n' "$bootstrap_parent" >&2
fi
TMPDIR=$bootstrap_dir
export TMPDIR
if [ -n "$package_url" ] && [ -n "$requested_package" ]; then
  remaining=$#; expect_package=false
  while [ "$remaining" -gt 0 ]; do
    arg=$1; shift
    if [ "$expect_package" = true ]; then arg=$bootstrap_dir/$archive; expect_package=false
    elif [ "$arg" = --package ]; then expect_package=true; fi
    set -- "$@" "$arg"; remaining=$((remaining - 1))
  done
fi
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
