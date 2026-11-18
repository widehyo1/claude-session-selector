#!/usr/bin/env bash
set -euo pipefail

selector_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

bin_dir="${CLAUDE_CLI_BIN_DIR:-$HOME/.local/bin}"

mkdir -p "$bin_dir"

cargo build --release --manifest-path "$selector_dir/Cargo.toml"

install -m 755 "$selector_dir/target/release/select-claude-session" "$bin_dir/select-claude-session"

printf 'installed to %s\n' "$bin_dir"
