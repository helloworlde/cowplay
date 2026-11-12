#!/bin/sh
# A Linux-musl cross compiler and the matching official Rust target must be supplied.
set -eu
here=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
target=${1:-aarch64-unknown-linux-musl}
cargo build --manifest-path "$here/Cargo.toml" --locked --release --target "$target"
