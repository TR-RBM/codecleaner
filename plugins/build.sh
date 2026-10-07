#!/bin/sh
# Builds all plugins and puts them where codecleanup embeds them.
# Needs: rustup target add wasm32-unknown-unknown
set -eu
cd "$(dirname "$0")"
cargo build --release --target wasm32-unknown-unknown
cp target/wasm32-unknown-unknown/release/codecleanup_lua.wasm ../languages/lua.wasm
cp target/wasm32-unknown-unknown/release/codecleanup_typescript.wasm ../languages/typescript.wasm
