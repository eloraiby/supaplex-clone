# Independent animation and collision traces

These fixtures come from OpenSupaplex commit
[`bad56a4e174e628643995284ea55d4c49af3137c`](https://github.com/sergiou87/open-supaplex/tree/bad56a4e174e628643995284ea55d4c49af3137c).
The C harness includes its real `supaplex.c` and calls `updateMovingObjects`.
It does not reimplement actor transitions or calculate expected coordinates.

`generate_traces.py` instruments `drawMovingSpriteFrameInLevel` at entry to record
its source rectangle and destination. The rest of the reference implementation
is unchanged. Null audio/video backends allow headless execution. Each case runs
in a fresh process, with Murphy and fixture cells initialized before its first
update. The scenarios contain no cadence-dependent Bugs, Terminals, or idle
animation. SDL2 supplies the upstream input/platform dependencies.

To reproduce with Python 3, a C compiler, and SDL2 development files:

```sh
git clone https://github.com/sergiou87/open-supaplex.git /tmp/open-supaplex-oracle
git -C /tmp/open-supaplex-oracle checkout bad56a4e174e628643995284ea55d4c49af3137c
python3 tests/support/generate_traces.py /tmp/open-supaplex-oracle
cargo test render::level
```

The generator builds in a temporary directory and does not modify the upstream
checkout. It overwrites only the four `opensupaplex_*_trace.txt` fixture files in
this directory. Its graphics hook reads the pinned Git object, so an existing
local hook cannot be applied twice. Use a checkout whose other source files are
unmodified.

Each `BLIT` contains tick, source x/y, width/height, and destination x/y, all in
original pixels before the reference viewport's border offset. `STATE` captures
board state after all callbacks in that tick. Coordinates in the Rust bitmap
include the full board, so the logged tile coordinates can be used directly.

| Fixture | Scenario | Assertions |
| --- | --- | --- |
| `opensupaplex_snap_trace.txt` | Murphy (4,3), one adjacent Base/Infotron/Red Disk, hardware elsewhere; hold Space+direction for eight ticks | Exact ordered copies, target tile, collection timing; separate pixel regression checks the entire cleared target |
| `opensupaplex_walk_trace.txt` | Eat adjacent Base for eight ticks, reverse for eight, cross the cleared Base for eight | Complete saved bitmap and Murphy position after every tick, all four directions |
| `opensupaplex_push_trace.txt` | Push Zonk onto RAM, keep holding toward its roll/fall for 48 ticks | Complete saved bitmap and Murphy position each tick, both directions |
| `opensupaplex_follow_trace.txt` | Eat Base while a Zonk/Infotron rolls away, follow, reverse through the eaten Base, follow again | Complete saved bitmap and Murphy position each tick; both actors and directions |

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

The renderer tests now use ordinary `Game::tick` updates and compare consecutive
typed cell buffers. The snap fixture checks the bounded sprites selected from
those pairs; the walk, push, and follow fixtures compare the complete resulting
bitmap after each tick. Production code consumes each cell's sprites immediately
and swaps the two reusable buffers when the frame is complete. The test-only
collection used for literal `BLIT` comparisons is not a simulation graphics queue.

These fixtures and demo hashes were kept unchanged when replacing the Drawing
API with cell-buffer rendering. Separate tests cover buffer reuse, repeated frame
submission, immediate commands without a tick, and Terminal sprite caching.
