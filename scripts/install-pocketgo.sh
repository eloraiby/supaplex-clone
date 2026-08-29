#!/bin/sh
set -eu

project_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
mount_root=${1:-/media/aifu/main}
package_root="$project_root/target/pocketgo-package"
source_game="$package_root/games/supaplex"
source_link="$package_root/gmenu2x/sections/games/Supaplex"
target_game="$mount_root/games/supaplex"
target_link_dir="$mount_root/gmenu2x/sections/games"

if [ ! -d "$mount_root/games" ] || [ ! -d "$target_link_dir" ]; then
    echo "Not a mounted PocketGo main partition: $mount_root" >&2
    echo "Expected games/ and gmenu2x/sections/games/ beneath it." >&2
    exit 1
fi

if [ ! -f "$source_game/supaplex-clone" ] \
    || [ ! -f "$source_game/run.dge" ] \
    || [ ! -f "$source_game/icon.png" ] \
    || [ ! -f "$source_link" ]; then
    echo "PocketGo package is incomplete: $package_root" >&2
    echo "Run scripts/build-pocketgo.sh first." >&2
    exit 1
fi

install -d -m 0755 "$target_game"
install -m 0755 "$source_game/supaplex-clone" "$target_game/supaplex-clone"
install -m 0755 "$source_game/run.dge" "$target_game/run.dge"
install -m 0644 "$source_game/icon.png" "$target_game/icon.png"
install -m 0644 "$source_link" "$target_link_dir/Supaplex"

# Remove the wrapper used by packages made before direct GMenu2X launching.
rm -f "$target_game/launch.sh"
sync

verify_copy() {
    source_file=$1
    target_file=$2
    label=$3

    if ! cmp -s "$source_file" "$target_file"; then
        echo "Installation verification failed for $label: $target_file" >&2
        exit 1
    fi
}

verify_copy "$source_game/supaplex-clone" "$target_game/supaplex-clone" "game binary"
verify_copy "$source_game/run.dge" "$target_game/run.dge" "diagnostic launcher"
verify_copy "$source_game/icon.png" "$target_game/icon.png" "menu icon"
verify_copy "$source_link" "$target_link_dir/Supaplex" "GMenu2X entry"

if ! grep -qx 'exec=/mnt/games/supaplex/run.dge' "$target_link_dir/Supaplex"; then
    echo "Installed GMenu2X entry has an unexpected executable path." >&2
    exit 1
fi

echo "Installed and byte-verified Supaplex on: $mount_root"
echo "  binary: $target_game/supaplex-clone"
echo "  launcher: $target_game/run.dge"
echo "  icon: $target_game/icon.png"
echo "  GMenu2X entry: $target_link_dir/Supaplex"
