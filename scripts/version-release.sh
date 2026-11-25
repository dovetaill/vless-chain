#!/usr/bin/env bash
# 从任意工作目录启动版本、构建、发布及运维菜单。
set -Eeuo pipefail
script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
export CHAIN_TASK_REPO=$(cd -- "$script_dir/.." && pwd)
command -v cargo >/dev/null || { echo '源码构建需要 Rust；服务器安装请使用 scripts/install-server.sh。' >&2; exit 1; }
task_parent=${RUNNER_TEMP:-${TMPDIR:-/tmp}}
task_previous=
for task_argument in "$@"; do
    if [ "$task_previous" = --work-parent ]; then task_parent=$task_argument; fi
    task_previous=$task_argument
done
task_bootstrap=$(mktemp -d "$task_parent/.chain-bootstrap-XXXXXXXX")
task_pid=
set -m
cleanup() {
    if [ -n "$task_pid" ]; then
        kill -TERM -- "-$task_pid" 2>/dev/null || true
        for task_attempt in {1..50}; do
            kill -0 -- "-$task_pid" 2>/dev/null || break
            sleep 0.1
        done
        kill -KILL -- "-$task_pid" 2>/dev/null || true
        wait "$task_pid" 2>/dev/null || true
    fi
    rm -rf -- "$task_bootstrap"
    printf '启动目录已清理：%s\n' "$task_bootstrap"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
export CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0
if [ "${GITHUB_ACTIONS:-}" = true ] && [ "${CHAIN_BUILD_CACHE:-}" = 1 ]; then
    export CARGO_TARGET_DIR="$RUNNER_TEMP/chain-build-cache/target"
    export CARGO_HOME="$RUNNER_TEMP/chain-build-cache/cargo"
else
    export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$task_bootstrap/target}"
    export CARGO_HOME="${CARGO_HOME:-$HOME/.cargo}"
fi
mkdir -p "$CARGO_HOME" "$CARGO_TARGET_DIR"
cargo build --locked --manifest-path "$CHAIN_TASK_REPO/panel/Cargo.toml" --bin xtask &
task_pid=$!
wait "$task_pid"
task_pid=
"$CARGO_TARGET_DIR/debug/xtask" "$@" <&0 &
task_pid=$!
if [ -t 0 ]; then
    fg %+ >/dev/null
else
    wait "$task_pid"
fi
task_pid=
