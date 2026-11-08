#!/usr/bin/env bash
# Build hive from this checkout and install it with cargo.
set -euo pipefail
cd "$(dirname "$0")"
if ! command -v cargo >/dev/null; then
  echo "hive needs Rust: https://rustup.rs" >&2
  exit 1
fi
cargo install --path . --locked --force
command -v claude >/dev/null || echo "note: the Claude Code CLI (claude) isn't on your PATH; hive drives it to run agents."
dir="${CARGO_HOME:-$HOME/.cargo}/bin"
case ":$PATH:" in
  *":$dir:"*) echo "installed: run \`hive\`" ;;
  *) echo "installed to $dir — add it to your PATH: export PATH=\"$dir:\$PATH\"" ;;
esac
