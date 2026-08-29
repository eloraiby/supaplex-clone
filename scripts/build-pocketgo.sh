#!/bin/sh
set -eu

project_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
miyoo_sdk=${MIYOO_SDK:-"$project_root/miyoo"}
target=armv5te-unknown-linux-uclibceabi
linker="$miyoo_sdk/bin/arm-miyoo-linux-uclibcgnueabi-gcc"

if [ ! -x "$linker" ]; then
    echo "PocketGo linker not found: $linker" >&2
    echo "Extract the MiyooCFW 1.3.3 toolchain into $project_root/miyoo" >&2
    echo "or set MIYOO_SDK to another extracted miyoo directory." >&2
    exit 1
fi

if ! rustup component list --toolchain nightly 2>/dev/null | grep -q '^rust-src (installed)'; then
    echo "nightly rust-src is required: rustup component add rust-src --toolchain nightly" >&2
    exit 1
fi

cd "$project_root"
CARGO_TARGET_ARMV5TE_UNKNOWN_LINUX_UCLIBCEABI_LINKER="$linker" \
CARGO_PROFILE_RELEASE_OPT_LEVEL=3 \
RUSTFLAGS="${RUSTFLAGS:-} -C target-cpu=arm926ej-s" \
    cargo +nightly build \
        -Z build-std=std,panic_abort \
        --target "$target" \
        --release \
        --features pocketgo

package_root="$project_root/target/pocketgo-package"
archive="$project_root/target/supaplex-pocketgo.zip"
mkdir -p \
    "$package_root/games/supaplex" \
    "$package_root/gmenu2x/sections/games"
install -m 0755 \
    "$project_root/target/$target/release/supaplex-clone" \
    "$package_root/games/supaplex/supaplex-clone"
install -m 0755 \
    "$project_root/packaging/pocketgo/run.dge" \
    "$package_root/games/supaplex/run.dge"
install -m 0644 \
    "$project_root/packaging/pocketgo/icon.png" \
    "$package_root/games/supaplex/icon.png"
install -m 0644 \
    "$project_root/packaging/pocketgo/Supaplex" \
    "$package_root/gmenu2x/sections/games/Supaplex"
rm -f "$package_root/games/supaplex/launch.sh"

cd "$package_root"
zip -FS -q -r "$archive" games gmenu2x
echo "PocketGo package: $archive"
