#!/usr/bin/env python3
"""Regenerate independent OpenSupaplex traces without modifying its checkout.

Usage: python3 tests/support/generate_traces.py /path/to/open-supaplex
Requires a C compiler, SDL2 development files, and the pinned upstream commit.
"""

from pathlib import Path
import subprocess
import sys
import tempfile

# Pin the implementation rather than silently changing the test oracle with HEAD.
REVISION = "bad56a4e174e628643995284ea55d4c49af3137c"
SUPPORT = Path(__file__).resolve().parent


def run(*args, **kwargs):
    """Return captured output, reporting compiler diagnostics only on failure."""
    try:
        return subprocess.check_output(args, text=True, stderr=subprocess.PIPE, **kwargs)
    except subprocess.CalledProcessError as error:
        sys.stderr.write(error.stderr)
        raise


def main():
    """Compile upstream with one draw-call hook and write the nine fixture suites."""
    upstream = Path(sys.argv[1]).resolve()
    assert run("git", "-C", str(upstream), "rev-parse", "HEAD").strip() == REVISION
    with tempfile.TemporaryDirectory(prefix="supaplex-trace-") as directory:
        build = Path(directory)
        # Read the pinned file directly, avoiding any earlier local instrumentation.
        graphics = run("git", "-C", str(upstream), "show", f"{REVISION}:src/graphics.c")
        start = graphics.index("void drawMovingSpriteFrameInLevel(")
        body = graphics.index("{", start) + 1
        hook = '\n    extern int referenceTick;\n    printf("BLIT %d %d %d %d %d %d %d\\n", referenceTick, srcX, srcY, width, height, dstX, dstY);\n'
        (build / "graphics.c").write_text(graphics[:body] + hook + graphics[body:])
        sources = [p for p in (upstream / "src").glob("*.c") if p.name not in ("supaplex.c", "graphics.c")]
        sources += [upstream / "src" / name for name in (
            "null/audio.c", "null/video.c", "null/virtualKeyboard.c",
            "sdl2/controller.c", "sdl2/keyboard.c", "sdl2/touchscreen.c",
            "sdl_common/system.c", "lib/ini/ini.c",
        )]
        sources += [build / "graphics.c", SUPPORT / "opensupaplex_trace.c"]
        flags = run("sdl2-config", "--cflags").split()
        objects = []
        for index, source in enumerate(sources):
            obj = build / f"{index}.o"
            run("cc", "-O2", "-DHAVE_SDL2", *flags, "-I", str(upstream), "-I", str(upstream / "src"), "-c", str(source), "-o", str(obj))
            objects.append(str(obj))
        executable = build / "trace"
        run("cc", *objects, *run("sdl2-config", "--libs").split(), "-lm", "-o", str(executable))
        special = [
            ("snik", "snik_turn", "Snik Snak turns and begins a movement transfer."),
            ("electron", "electron_turn", "Electron turns and begins a movement transfer."),
            ("bug", "bug_cooldown", "Bug advances on quarter ticks and enters its safe interval without a copy."),
            ("orange", "falling_orange", "Orange Disk falls into Hardware and detonates."),
            ("blast", "falling_zonk", "Falling Zonk collides with stationary Murphy."),
        ]
        for mode, case, description in special:
            lines = [
                f"# Captured from OpenSupaplex {REVISION}.",
                f"# {description} Quarter-rate callbacks use the original global frame counter.",
                "",
                f"CASE {case}",
                run(str(executable), mode, "0", "0").strip(),
                "",
            ]
            (SUPPORT / f"opensupaplex_{mode}_trace.txt").write_text("\n".join(lines))
        directions = [("up", 1), ("left", 2), ("down", 3), ("right", 4)]
        for mode, tiles in [("snap", [4, 2, 20]), ("walk", [2]), ("push", [1]), ("follow", [1, 4])]:
            lines = [f"# Captured from OpenSupaplex {REVISION}.", "# See opensupaplex_traces.md for reproduction and scenario definitions.", ""]
            for tile in tiles:
                for direction, code in directions:
                    if mode in ("push", "follow") and direction not in ("left", "right"):
                        continue
                    case = f"{tile} {direction}" if mode in ("snap", "follow") else direction
                    lines.extend([f"CASE {case}", run(str(executable), mode, str(tile), str(code)).strip(), ""])
            (SUPPORT / f"opensupaplex_{mode}_trace.txt").write_text("\n".join(lines))


if __name__ == "__main__":
    main()
