# Supaplex clone

This project plays the 111 original Supaplex levels in SDL2. It loads the
bundled DOS `LEVELS.DAT`, simulates actor-owned animation and behavior, and
renders the supplied `RocksSP.png` sprite atlas with nearest-neighbor scaling.

## Run a level

Install Rust and an SDL2 development library discoverable through
`pkg-config`, then select a one-based level number:

```bash
cargo run --release -- --level 1
```

Valid numbers are `1..=111`. The executable embeds the supplied level set and
rendering assets, so it does not depend on the process working directory after
it is built.

Controls:

- Arrow keys: move Murphy.
- Space + arrow: eat Base or collect an adjacent Infotron/Red Disk without
  moving.
- Space without an arrow, or `D`: plant one collected Red Disk beneath Murphy;
  move away before its fuse expires. Only one planted fuse can be active.
- `R`: restart the selected level from its original record.
- `Escape`: quit.

The HUD shows the original level title, remaining Infotrons, Red Disk inventory,
gravity, and the Zonk freeze state. The camera follows Murphy across the full
60×24 board.

## Simulation design

`Board` stores exactly one contiguous `Vec<State>`. Its sole coordinate
conversion is:

```text
index = width * y + x
```

Each `State` combines an `Actor` with a validated `Animation`. `Actor` is an
enum of concrete actor structs (`Murphy`, `Zonk`, `Infotron`, `Port`, and so on),
and its dispatch method calls the transition method belonging to that concrete
actor. An animation stores its current frame, duration, and promised terminal
action—settle, act, repeat, explode, disappear, or become an Infotron.

Every fixed tick follows the original deterministic linear order:

1. Murphy inspects and immediately updates the live board first.
2. The engine captures a row-major list directly from that post-Murphy board.
3. Each scheduled updater verifies that its actor still occupies the cell,
   reads the board left by earlier updates, and applies all of its writes and
   events before the next updater runs.
4. An actor moved by a scheduled callback is not called again at its new
   destination. Actors pushed by Murphy are already present when the list is
   captured, so they receive their normal callback in that pass.

There is no competing-move resolution phase: the update direction is linear,
and every later callback observes mutations made by every earlier callback.

Logical movement occupies the destination at animation start while its source
becomes an invisible `Vacating` reservation. The destination sprite is offset
toward that source until both synchronized animations finish. Murphy releases
his old cell while retaining the final moving pose, and processes new direction
input on his next update. A trailing Zonk that sees the newly opened cell later
in the same pass first enters `ZonkPreFall`; it transfers only on the following
pass, after Murphy has received that next input.

Implemented play mechanics include Base removal, Infotron and Red Disk
collection, locked exits, horizontal Zonk and Orange Disk pushing, player
gravity, falling and rolling Zonks/Infotrons, falling Orange Disks, one-way and
multi-way ports, special-port toggles, Bug timing, Snik Snaks, Electrons,
Terminal/Yellow Disk detonation, single-fuse planted Red Disks, merged and
chained 3×3 explosions, Electron-to-Infotron residue, synchronized initial Bug
cycles with randomized per-Bug cooldowns, animated player death,
completion, and restart. The current scope intentionally omits menus, profiles,
demos, sound, and progression between levels.

## Original data and PNG conversion

The graphics `.DAT` files are not PNG streams with only a missing signature.
They are headerless DOS bitmaps: gameplay sheets use four MSB-first bitplanes,
fonts use one bit per pixel, and dimensions live outside each file. The
`dat-to-png` utility decodes those planes with `PALETTES.DAT` and then writes a
real PNG header and compressed image stream.

The committed files in `assets/` can be reproduced with:

```bash
cargo run --bin dat-to-png -- data/fixed.dat assets/fixed.png
cargo run --bin dat-to-png -- data/moving.dat assets/moving.png
cargo run --bin dat-to-png -- data/chars8.dat assets/chars8.png
```

`FIXED.DAT` becomes 640×16, `MOVING.DAT` becomes 320×462, and `CHARS8.DAT`
becomes 512×8. Gameplay uses the easier-to-map 16×15 grid of 32×32 frames in
`RocksSP.png`; the HUD uses the converted original font.

## Verification

```bash
cargo test
cargo clippy --all-targets -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps
```

Tests cover the CLI range, complete record layout and metadata, row-major board
indexing, every supplied level, actor movement and collection, gravity
overrides, falling-object collision matrices, Red Disk planting, ordered and
chained explosions, Bug timing/RNG, exit gating, planar and binary graphics
decoding, PNG assets, and atlas bounds.

Format and mapping references:

- [Historical Supaplex file formats](https://www.elmerproductions.com/sp/filefmt.html)
- [OpenSupaplex tile and level definitions](https://github.com/sergiou87/open-supaplex/blob/master/src/globals.h)
- [Superplexed planar graphics decoder](https://github.com/kaimitai/superplexed)
