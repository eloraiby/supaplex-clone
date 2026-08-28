# Supaplex clone

This project plays the 111 original Supaplex levels in SDL2. It opens with the
original title and main-menu artwork, loads the DOS `LEVELS.DAT`, simulates
actor-owned animation and behavior, and renders pixel-perfect conversions of
the original graphics with nearest-neighbor scaling. The original AdLib music
and Sound Blaster gameplay effects play through a self-contained SDL mixer.

## Run a level

Install Rust and an SDL2 development library discoverable through `pkg-config`,
then launch the front end:

```bash
cargo run --release
```

The optional `--level <1-111>` argument chooses the initially highlighted menu
row; omitting it starts at level 1. The default executable embeds every runtime
asset, so it does not depend on the process working directory after it is built.
The optional `--step <5-60>` argument selects fixed simulation updates per second;
omitting it preserves the original rate of 35 updates per second.

### Unbundled build

Enable the `unbundle` feature to keep levels, graphics, music, and effects out
of the executable:

```bash
cargo run --release --features unbundle
```

An unbundled build reads the following distribution tree at startup:

```text
assets/data/levels.dat
assets/data/demo{0,1,2,3,4,5,6,7,8,9}.bin
assets/gfx/{fixed,moving,chars8,chars6,title,menu,gfx,controls,back,panel}.png
assets/audio/music.xm
assets/audio/{explosion,infotron,push,fall,bug,base,exit}.wav
```

Paths are relative to the working directory by default. Set
`SUPAPLEX_ASSET_ROOT` to the directory containing `assets/` when launching the
executable from elsewhere.

Player names, completion time, level results, and the selected profile are saved
under SDL's per-user preference directory. Set `SUPAPLEX_PROFILE_PATH` to an
exact file path for a portable installation or an isolated test session.

Controls:

- Any key: advance past the title splash.
- Mouse: select every original main-menu and controls-screen button; hovered
  controls receive a visible outline.
- Menu arrows: select a level; `Page Up`/`Page Down` move ten levels, and
  `Home`/`End` select the first or last level. `N`, `Delete`, `K`, `T`, `G`,
  `D`, and `C` activate New Player, Delete Player, Skip Level, Statistics,
  GFX Tutor, Demo, and Controls respectively.
- `F1` through `F10`: play the corresponding original demonstration; any key or
  mouse button returns from a running demo to the menu.
- `Enter` or `Space`: start the highlighted menu level.
- Arrow keys: move Murphy.
- Space + arrow: eat Base or collect an adjacent Infotron/Red Disk without
  moving.
- Space without an arrow, or `D`: plant one collected Red Disk beneath Murphy;
  move away before its fuse expires. Only one planted fuse can be active.
- `R`: restart the selected level from its original record.
- `M`: mute or resume music.
- `S`: mute or enable sound effects.
- `Escape`: explode Murphy during gameplay, or quit from the menu.

The original bottom status panel shows the player, level number and title, game
time, remaining Infotrons, and Red Disk inventory. The camera follows Murphy
across the full 60×24 board. Menu-to-level and level-to-menu changes use the
original 64-frame, 70 Hz palette-fade duration. Finishing a level returns with
the following level highlighted; death returns with the same level selected.

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
2. The engine captures a row-major list directly from that post-Murphy board,
   using the DOS loop's literal `width + 1 .. cell_count - width - 1` bounds.
3. Each scheduled updater verifies that its actor still occupies the cell,
   reads the board left by earlier updates, and applies all of its writes and
   events before the next updater runs.
4. An actor moved by a scheduled callback is not called again at its new
   destination. Actors pushed by Murphy are already present when the list is
   captured, so they receive their normal callback in that pass.

There is no competing-move resolution phase: the update direction is linear,
and every later callback observes mutations made by every earlier callback.
The scan bounds are intentionally not rewritten as a geometric inner rectangle;
the original asymmetric edge behavior is required by known demos. Tile 40 is
the historical accidental invisible wall: it is always collision-solid, always
drawn as empty space, and has no reveal-on-touch transition.

Logical movement occupies the destination at animation start while its source
becomes an invisible `Vacating` reservation. The destination sprite is offset
toward that source and owns the reservation's release; temporary Space markers
do not receive autonomous original callbacks. Falling Zonks and Infotrons clear
their source on state `0x16`, before their last two pictures. Murphy releases his
old cell while retaining the final moving pose, and processes new direction
input on his next update. A trailing Zonk that sees the newly opened cell later
in the same pass first enters `ZonkPreFall`; it transfers only on the following
pass, after Murphy has received that next input. Explosions that replace a
moving or rolling Zonk/Infotron clear the reservation selected by that actor's
live movement phase. Murphy may collect only idle Infotrons when snapping or
moving up, left, or right; ordinary downward movement retains the original
tile-only collision check.

Implemented play mechanics include Base removal, Infotron and Red Disk
collection, locked exits, horizontal Zonk and Orange Disk pushing, player
gravity, falling and rolling Zonks/Infotrons, falling Orange Disks, one-way and
multi-way ports, special-port toggles, Bug timing, Snik Snaks, Electrons,
Terminal/Yellow Disk detonation, single-fuse planted Red Disks, merged and
chained 3×3 explosions, Electron-to-Infotron residue, synchronized initial Bug
cycles with randomized per-Bug cooldowns, animated player death,
completion, restart, and progression back to the next menu selection. Persistent
twenty-slot player profiles enforce the original completed/skipped/first-open
level progression and three-skip limit. The supplied legacy demos decode their
original run-length input at fixed-step boundaries and use the deterministic
zero seed expected by their standalone format.

## Runtime assets

The repository keeps only the files consumed by the game, grouped under one
distribution root:

- `assets/data/` contains the original level collection and demonstration inputs.
- `assets/gfx/` contains playback-ready RGBA PNG sheets and screens.
- `assets/audio/` contains the XM arrangement and playback-ready WAV effects.

The one-time DOS graphics and sound conversion inputs are intentionally not
shipped with the runtime tree. Gameplay still addresses the original fixed-tile
layout and variably sized moving descriptors within the converted PNG sheets.
The in-tree tracker player decodes the XM module's delta-compressed samples and
sequences its four channels directly in SDL's callback. Effects retain the
original one-channel priority rules, while music loops independently and pauses
when the Exit sound is accepted. The default build embeds the complete asset
tree; `unbundle` reads the same files from disk. Playback requires only SDL2,
not SDL2_mixer.

## Verification

```bash
cargo test
cargo clippy --all-targets -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps
```

Tests cover the CLI range, complete record layout and metadata, row-major board
indexing, every supplied level, actor movement and collection, gravity
overrides, falling-object collision matrices, Red Disk planting, ordered and
chained explosions, Bug timing/RNG, exit gating, PNG asset validation, audio
event timing, effect priorities, WAV decoding, XM parsing and playback, and
fixed-strip bounds.

Format and mapping references:

- [Historical Supaplex file formats](https://www.elmerproductions.com/sp/filefmt.html)
- [OpenSupaplex tile and level definitions](https://github.com/sergiou87/open-supaplex/blob/master/src/globals.h)
