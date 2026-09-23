# Supaplex clone

This project plays the 111 original Supaplex levels. It opens with the
original title and main-menu artwork, loads the DOS `LEVELS.DAT`, simulates
actor-owned animation and behavior, and renders pixel-perfect conversions of
the original graphics with nearest-neighbor scaling. Desktop builds use SDL2;
the original PocketGo uses its Linux framebuffer, keypad, and ALSA audio device
directly. The original AdLib music and Sound Blaster gameplay effects
play through the same self-contained mixer on both backends.

## Run a level

Install Rust and an SDL2 development library discoverable through `pkg-config`,
then launch the front end:

```bash
cargo run --release
```

The optional `--level <1-111>` argument launches that level directly, bypassing
the title, menu, player profiles, and progression checks. The process exits when
the level ends. Omitting it opens the normal front end with level 1 highlighted.
The default executable embeds every runtime asset, so it does not depend on the
process working directory after it is built.
The optional `--step <5-60>` argument selects fixed simulation updates per second;
omitting it uses 50 updates per second.

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
assets/audio/{ADLIB,BLASTER}.SND
```

Paths are relative to the working directory by default. Set
`SUPAPLEX_ASSET_ROOT` to the directory containing `assets/` when launching the
executable from elsewhere.

Player names, completion time, level results, and the selected profile are saved
under SDL's per-user preference directory. Set `SUPAPLEX_PROFILE_PATH` to an
exact file path for a portable installation or an isolated test session.

## WebAssembly build

The browser build uses Rust's `wasm32-unknown-emscripten` target and
Emscripten's SDL2 port. It embeds the same levels, graphics, music, and effects
as the default desktop executable. Player profiles and level progress are stored
locally in the browser with IndexedDB.

Install and activate Emscripten 6.0.5, add the Rust target, and build the site:

```bash
rustup target add wasm32-unknown-emscripten
source /path/to/emsdk/emsdk_env.sh
./scripts/build-web.sh
```

The deployable output is written to `target/web/`. WebAssembly must be served
over HTTP rather than opened directly from the filesystem. For a local run:

```bash
python3 -m http.server -d target/web 8000
```

Then open `http://localhost:8000/`. Click the canvas once to focus keyboard
input and satisfy browser audio-autoplay rules.

The `Build and deploy WebAssembly` workflow builds the same site after every
push to `master` and publishes it with GitHub Pages. In the repository's GitHub
settings, choose **Pages → Build and deployment → Source: GitHub Actions** once;
no deployment branch or checked-in build output is needed.

## Original PocketGo build

The PocketGo V1 reports `armv5tejl` and runs MiyooCFW's ARM/uClibc userspace.
Its build does not link SDL1 or SDL2: it writes 320×240 RGB565 frames directly
to `/dev/fb0`, reads the Miyoo kernel keyboard from `/dev/input/event*` when
evdev is enabled or from the active Linux console in medium-raw mode on the
original firmware, and streams 44.1 kHz signed 16-bit stereo audio through the
firmware's ALSA PCM device. Set `SUPAPLEX_ALSA_DEVICE` to override the default
PCM name if a modified firmware exposes its playback device differently.

Install nightly Rust with its source component, then download and extract the
[MiyooCFW 1.3.3 toolchain](https://github.com/NxHope/miyoo_dev/releases/download/v1.3.3/toolchain.7z).
Place the extracted SDK in the project-local `miyoo/` directory. That directory
is intentionally ignored by Git because the complete SDK is roughly 905 MB.
`MIYOO_SDK` may still point to another extracted SDK when needed:

```bash
rustup toolchain install nightly
rustup component add rust-src --toolchain nightly
./scripts/build-pocketgo.sh
```

The handheld build keeps the game code optimized for the ARM926EJ-S while
rebuilding `std` with size-oriented routines and without its default backtrace
or panic-unwind features. Panics abort immediately at their call site: they do
not format a message, demangle symbols, collect a backtrace, run a panic hook,
or unwind destructors. The build script rejects an output binary that still
contains the backtrace runtime and prints the final executable size.

The script produces `target/supaplex-pocketgo.zip`. Extract that archive into
the root of the SD card's main data partition (mounted as `/mnt` by MiyooCFW),
then restart GMenu2X. The game appears in the Games section and stores progress
in `/mnt/games/supaplex/players.dat`.

To install an already-built package onto a main partition mounted on the build
machine, pass its mount point to the installer (the default is
`/media/aifu/main`):

```bash
./scripts/install-pocketgo.sh /path/to/mounted/main
```

The GMenu2X entry uses `run.dge`, a small diagnostic wrapper consistent with
other MiyooCFW games. It records loader/device checks, startup milestones, and
the process exit status in `/mnt/games/supaplex/supaplex.log`. The PocketGo
backend still resolves `players.dat` beside the executable without relying on
the wrapper environment.

PocketGo controls:

- D-pad: move or choose a menu row.
- A or Y: action; hold with a direction to snap, or press alone to plant a Red Disk.
- A or Start: confirm the selected level.
- B or Select: explode Murphy in-game; return/quit in menus.
- X: restart the current level; open Controls from the main menu.
- L1/R1: move ten levels in the menu; toggle music/effects during play.

The `pocketgo` Cargo feature can also compile-check this backend on a desktop
without opening the devices: `cargo check --features pocketgo`.

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

Each board `State` contains one complete `Actor`. The actor owns its legal
phases and progress; there is no separately assignable animation or completion
command. For example, a Zonk can be resting, awaiting a fall, rolling, falling,
or held by Murphy. Its falling phase carries `Frame<8>` and its rolling phase
requires `Horizontal`, so a Murphy action or an upward roll cannot be assigned
to a Zonk.

```rust
use supaplex_clone::actors::{Actor, Frame, State, Zonk, rounded::RoundedPhase};

let falling_rock = State::new(Actor::Zonk(Zonk::from_phase(
    RoundedPhase::Falling(Frame::first()),
)));
assert!(matches!(falling_rock.actor(), Actor::Zonk(rock)
    if matches!(rock.phase(), RoundedPhase::Falling(_))));
```

Each concrete actor lives under `src/actors/` and advances through an exhaustive
`match` on its own phase. `actors.rs` dispatches to those state machines and
re-exports their public types. Completion behavior follows from the phase:
finishing a Zonk fall invokes its landing rules directly. There is no
`AnimationNext`, `BeginZonkFall`, or generic actor/animation pairing constructor.
The former `actor` import path is now `supaplex_clone::actors`.

Shared responsibilities are separated by concern:

| File | Responsibility |
| --- | --- |
| `actors.rs` | Actor dispatch and typed collision queries |
| `actors/geometry.rs` | Board positions, cardinal directions, and horizontal-only directions |
| `actors/frame.rs` | Private, bounded `Frame<N>` values and safe advancement |
| `actors/rounded.rs` | Legal Zonk/Infotron phases, roll reservations, and shared falling mechanics |
| `actors/enemy.rs` | Legal enemy phases and eight-picture turn mapping |
| `actors/empty.rs` | Explicit source, side, and destination reservations |
| `render.rs` | Direct sprite selection from actor-specific phases and bounded frames |
| `actors/state.rs` | Complete cell values and level/session construction boundaries |
| `actors/transition.rs` | Owned atomic cell writes and game-session events |

Murphy's preparation, planting, movement, snapping, pushing, port traversal, and
exit sequence belong to one phase enum. They cannot coexist as independent
flags or animation commands. Push actions constrain rocks and Orange Disks to
horizontal directions. His seven-picture Infotron snap, eight-picture ordinary
travel, nine-picture rightward Red Disk travel, and forty-picture exit use
distinct bounded frame types. The final movement pose is an explicit resumption
phase, preserving the update between source release and fresh input.

Rendering matches the actual actor and its typed phase directly. There is no
shared animation-kind enum, generic animation object, or `State::animation()`
API. Enemy sprite selectors accept `EnemyPhase`; rock selectors accept
`RoundedPhase`; Bug and explosion tables take their own bounded frames.
Murphy's own artwork descriptor selects the original composite sprite tables.
Gameplay and rendering both read actor-owned state. `Frame<N>`
rejects external indices outside its strip, and its private representation
prevents unchecked construction. Runtime checks still handle board occupancy,
bounds, and reservations replaced by earlier actors; these depend on the live
world rather than on an individual actor's type.

Actor callbacks take `&self` because they compute owned replacement states from
an immutable world view. The game applies transitions with exclusive mutable
access to the board. No `Any`, downcasting, `Cell`, or `RefCell` is needed.
When adding an actor, define its legal phases and completion behavior in its
own module, add its typed sprite selector, and wire its identity into dispatch and
collision queries. Scheduling, shared RNG, and the position-owned planted Red
Disk fuse remain in `game.rs`.

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
in the same pass first enters `RoundedPhase::AwaitingFall`; it transfers only on the following
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
- `assets/audio/` contains the original `ADLIB.SND` and `BLASTER.SND` drivers.

The one-time DOS graphics conversion inputs are intentionally not shipped with
the runtime tree. Gameplay still addresses the original fixed-tile
layout and variably sized moving descriptors within the converted PNG sheets.
The player reads the exact original 5,354-byte `ADLIB.SND` at runtime. A safe
Rust port of its DOS music routines interprets the file's compact pattern
bytecode, instrument definitions, frequency tables, tempo changes, transposes,
and pattern loops at the original 50 Hz interrupt rate, sending each resulting
register operation directly to a native Rust YM3812 emulator. The exact original
39,195-byte `BLASTER.SND` remains the sole effect source: startup validates the
complete driver, extracts its seven embedded Creative VOC records, and converts
their unsigned 8-bit mono PCM from 8,333⅓ Hz directly to the callback layout.
Effects retain the original one-channel priority rules, while music loops
independently and pauses when the Exit sound is accepted. The default build
embeds the complete asset tree; `unbundle` reads the same files from disk.
Playback requires only SDL2, not SDL2_mixer.

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
event timing, effect priorities, direct `BLASTER.SND` VOC extraction, direct
`ADLIB.SND` validation, sequencing and synthesis, and fixed-strip bounds.
Compile-fail documentation tests reject mismatched actor phases, vertical rolls
and rock pushes, wrong-length frame payloads, unchecked frame construction, and
arbitrary actor/animation pairing. `tests/demo_replay.rs` compares all ten demo
histories with pre-refactor fixtures over 46,199 input ticks, including every
cell's sprite timing and collision state, game counters, and emitted sounds.
These fixtures preserve existing behavior; they do not assert that every legacy
demo currently completes its level. Rendering tests also compare all 182 original
gravity, enemy, Bug, and explosion rectangles with a pre-refactor fingerprint,
and check terrain layering and camera interpolation directly from typed phases.

Source release and animation completion are distinct events. In
[OpenSupaplex's rounded-object update routines](https://github.com/sergiou87/open-supaplex/blob/master/src/supaplex.c),
roll sources release at states `0x26`/`0x36` and falling sources at `0x16`,
before their final pictures. The routines draw the current picture before
advancing the state, and the preparation pictures belong to the roll's eight
pictures. These counters must not be treated as interchangeable with a
post-update snapshot frame without checking the full transition sequence.

Known rendering issue: when Murphy follows a pushed Zonk through a roll and
fall, opaque animation erase rectangles can paint over him. The gameplay
following window must be preserved when correcting this presentation issue.
The replay fingerprints verify existing behavior, not full OpenSupaplex parity
or the correctness of every rendered frame.

Format and mapping references:

- [Historical Supaplex file formats](https://www.elmerproductions.com/sp/filefmt.html)
- [OpenSupaplex tile and level definitions](https://github.com/sergiou87/open-supaplex/blob/master/src/globals.h)

The historical replay fixture encoding lives only in `tests/support/legacy_snapshot.rs`.
It translates typed state into the original fixture text so the pre-refactor
fingerprints remain unchanged; production simulation and rendering do not use it.
