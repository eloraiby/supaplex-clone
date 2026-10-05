# Independent animation and collision traces

These fixtures come from OpenSupaplex commit
[`bad56a4e174e628643995284ea55d4c49af3137c`](https://github.com/sergiou87/open-supaplex/tree/bad56a4e174e628643995284ea55d4c49af3137c).
The C harness includes its real `supaplex.c` and calls `updateMovingObjects`.
It does not reimplement actor transitions or calculate expected coordinates.

`generate_traces.py` instruments `drawMovingSpriteFrameInLevel` at entry to record
its source rectangle and destination. The rest of the reference implementation
is unchanged. Null audio/video backends allow headless execution. Each case runs
in a fresh process, with Murphy and fixture cells initialized before its first
update. The enemy cases begin in turn state one, matching the original
pre-play conversion when their left neighbor is free. The snap, walk, push, and
follow scenarios contain no cadence-dependent Bugs, Terminals, or idle animation.
The blast, Orange Disk, Bug, Snik Snak, and Electron scenarios advance the
upstream frame counter and explosion timers to check their distinct cadences.
SDL2 supplies the upstream input/platform dependencies.

To reproduce with Python 3, a C compiler, and SDL2 development files:

```sh
git clone https://github.com/sergiou87/open-supaplex.git /tmp/open-supaplex-oracle
git -C /tmp/open-supaplex-oracle checkout bad56a4e174e628643995284ea55d4c49af3137c
python3 tests/support/generate_traces.py /tmp/open-supaplex-oracle
cargo test render::level
```

The generator builds in a temporary directory and does not modify the upstream
checkout. It overwrites only the nine `opensupaplex_*_trace.txt` fixture files in
this directory. Its graphics hook reads the pinned Git object, so an existing
local hook cannot be applied twice. Use a checkout whose other source files are
unmodified.

Each `BLIT` contains tick, source x/y, width/height, and destination x/y, all in
original pixels before the reference viewport's border offset. `STATE` marks
the end of a tick and includes board fields in scenarios that check occupancy.
Coordinates in the Rust bitmap include the full board, so the logged tile
coordinates can be used directly.

| Fixture | Scenario | Assertions |
| --- | --- | --- |
| `opensupaplex_snap_trace.txt` | Murphy (4,3), one adjacent Base/Infotron/Red Disk, hardware elsewhere; hold Space+direction for eight ticks | Exact ordered copies, target tile, collection timing; separate pixel regression checks the entire cleared target |
| `opensupaplex_walk_trace.txt` | Eat adjacent Base for eight ticks, reverse for eight, cross the cleared Base for eight | Complete saved bitmap and Murphy position after every tick, all four directions |
| `opensupaplex_push_trace.txt` | Push Zonk onto RAM, keep holding toward its roll/fall for 48 ticks | Complete saved bitmap and Murphy position each tick, both directions |
| `opensupaplex_follow_trace.txt` | Eat Base while a Zonk/Infotron rolls away, follow, reverse through the eaten Base, follow again | Complete saved bitmap and Murphy position each tick; both actors and directions |
| `opensupaplex_blast_trace.txt` | A falling Zonk collides with stationary Murphy | Complete saved bitmap over 48 ticks, including every explosion picture and its first-copy delay |
| `opensupaplex_orange_trace.txt` | An Orange Disk falls into Hardware and detonates | Saved pixels around the disk and blast over 65 ticks, including the last explosion picture |
| `opensupaplex_bug_trace.txt` | A Bug completes its active cycle and enters a safe interval | Saved Bug pixels over 60 ticks; the safe transition makes no copy |
| `opensupaplex_snik_trace.txt` | A Snik Snak turns and moves into a free cell | Saved enemy pixels over 48 ticks, including the draw-before-increment turn and transfer boundary |
| `opensupaplex_electron_trace.txt` | An Electron follows the same turn and movement sequence | Saved enemy pixels over 48 ticks |

The push fixture specifically requires Murphy to wait through tick 21 and enter
the released rolling source on tick 22. The broken transition allowed entry on
tick 18, before the rock's later opaque pictures had cleared that area. The first
two pictures belong to roll preparation, not an additional delay before another
eight pictures.

Snapping Infotrons draws `(304,148,16,16)` on tick 7, then completes collection in
that same callback. Skipping that rectangle leaves ten colored pixels from tick
6. The test rejects an additional fixed-tile erase as well as a missing picture.

The original clone replay hashes cannot serve as an oracle for these corrections:
they encoded the early source release and old rounded timing. Updated hashes in
`demo_replay.rs` guard the corrected behavior across all bundled inputs. They do
not replace these independent reference traces or claim full upstream parity.

The renderer tests use ordinary `Game::tick` updates and compare consecutive
previous/current boards. The snap fixture checks the bounded sprites selected
from cell pairs; the walk, push, follow, blast, Orange Disk, Bug, and enemy
fixtures compare saved pixels after each tick. Display refreshes never repeat
those copies.

The snapshot renderer preserves opaque ordering and the original explosion
first-copy delay. Separate tests cover buffer reuse, repeated frame submission,
immediate commands between ticks, the planted Red Disk's independent copy, and
Terminal sprite caching.
