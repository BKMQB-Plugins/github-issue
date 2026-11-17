#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

cargo fmt --all -- --check
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
cargo clippy --locked --target wasm32-unknown-unknown -- -D warnings
cargo build --locked --release --target wasm32-unknown-unknown
mkdir -p target/bkmqb
wasm-tools component new \
  target/wasm32-unknown-unknown/release/bkmqb_plugin_github_issue.wasm \
  -o target/bkmqb/component.wasm
wasm-tools validate target/bkmqb/component.wasm
plugin_id="$(sed -n '/^id = / { s/^id = "\(.*\)"/\1/p; q; }' plugin.toml)"
version="$(sed -n 's/^version = "\(.*\)"/\1/p' plugin.toml)"
package="${plugin_id}-${version}.bkm-plugin"
staging="$(mktemp -d target/bkmqb/package.XXXXXX)"
trap 'rm -rf "$staging"' EXIT
zip -X -j "$staging/$package" plugin.toml target/bkmqb/component.wasm \
  config.schema.json management.schema.json README.md
unzip -t "$staging/$package"
mv "$staging/$package" "target/bkmqb/$package"
sha256sum "target/bkmqb/$package" > "target/bkmqb/${package}.sha256"
