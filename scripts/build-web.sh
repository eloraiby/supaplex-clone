#!/usr/bin/env bash

set -euo pipefail

project_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
target="wasm32-unknown-emscripten"
binary_name="supaplex-clone"
wasm_name="supaplex_clone.wasm"
release_dir="$project_root/target/$target/release"
site_dir="$project_root/target/web"

if ! command -v emcc >/dev/null 2>&1; then
    echo "error: emcc is not available; activate an Emscripten SDK first" >&2
    exit 1
fi

if ! rustup target list --installed | grep -qx "$target"; then
    echo "error: Rust target $target is not installed" >&2
    echo "install it with: rustup target add $target" >&2
    exit 1
fi

cd "$project_root"
cargo build --release --target "$target"

mkdir -p "$site_dir"
cp "$project_root/web/index.html" "$site_dir/index.html"
cp "$project_root/packaging/pocketgo/icon.png" "$site_dir/icon.png"
cp "$release_dir/$binary_name.js" "$site_dir/$binary_name.js"
cp "$release_dir/$wasm_name" "$site_dir/$wasm_name"

echo "Web build written to $site_dir"
echo "Serve it over HTTP, for example: python3 -m http.server -d $site_dir 8000"
