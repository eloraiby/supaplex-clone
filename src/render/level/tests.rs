//! Verify cell-frame resolution and saved pixels independently of SDL frame cadence.

use super::*;
use crate::render::{SourcePoint, SpritePart, zonk_sprite_part};
use crate::{
    actors::{Direction, State, rounded::RoundedPhase},
    assets,
    game::{Game, Input},
    level::{LEVEL_RECORD_SIZE, LEVEL_WIDTH, Level, LevelSet},
};

/// Collects bounded cell sprites solely for comparison with the upstream fixture.
fn sprites_in_frame(
    previous: &Board,
    current: &Board,
    freeze_zonks: bool,
) -> Vec<(Position, Blit)> {
    let mut sprites = Vec::new();
    if let Some((position, parts)) = murphy_sprites(previous, current) {
        sprites.extend(parts.iter().map(|part| (position, part)));
    }
    for index in 0..current.cells().len() {
        let position = current.position(index).unwrap();
        sprites.extend(
            cell_sprites(previous, current, position, freeze_zonks)
                .iter()
                .map(|part| (position, part)),
        );
    }
    sprites
}

/// Loads the production atlases so erase pixels are tested without substitutes.
fn bitmap() -> LevelBitmap {
    let graphics = assets::load_graphics().unwrap();
    LevelBitmap::new(graphics.fixed.as_ref(), graphics.moving.as_ref()).unwrap()
}

/// Pushes a supported rock onto RAM so holding input follows its roll and fall.
fn push_level(direction: Direction) -> Level {
    let mut record = vec![0; LEVEL_RECORD_SIZE];
    record[..60 * 24].fill(6);
    for y in 1..7 {
        for x in 1..9 {
            record[y * LEVEL_WIDTH + x] = 0;
        }
    }
    let (start, support) = match direction {
        Direction::Left => (5, 3),
        Direction::Right => (3, 5),
        _ => unreachable!("fixture only supports horizontal pushes"),
    };
    record[2 * LEVEL_WIDTH + start] = 3;
    record[2 * LEVEL_WIDTH + 4] = 1;
    record[3 * LEVEL_WIDTH + 4] = 6;
    record[3 * LEVEL_WIDTH + support] = 5;
    LevelSet::new(&record).load(1).unwrap()
}

/// Reads an original-resolution RGBA pixel without camera or backend scaling.
fn pixel(image: &DecodedPng, x: usize, y: usize) -> &[u8] {
    let offset = (y * image.width as usize + x) * 4;
    &image.pixels[offset..offset + 4]
}

/// Display refreshes, camera movement, and restart preserve the bitmap contract.
#[cfg(not(any(feature = "pocketgo", target_env = "uclibc", target_arch = "wasm32")))]
#[test]
fn sdl_displays_saved_pixels_without_replaying_simulation() {
    use super::super::{LOGICAL_HEIGHT, LOGICAL_WIDTH, Renderer, VIEW_HEIGHT};
    use crate::platform::{
        self,
        pixels::{Color, PixelFormatEnum},
    };
    platform::hint::set("SDL_VIDEODRIVER", "dummy");
    let sdl = platform::init().unwrap();
    let video = sdl.video().unwrap();
    let window = video
        .window("ordered bitmap regression", LOGICAL_WIDTH, LOGICAL_HEIGHT)
        .hidden()
        .build()
        .unwrap();
    let mut canvas = window.into_canvas().software().build().unwrap();
    let creator = canvas.texture_creator();
    let mut renderer = Renderer::new(&creator).unwrap();
    let level = push_level(Direction::Right);
    let mut game = Game::with_random_seed(&level, 0).unwrap();
    assert!(matches!(
        renderer.draw(&mut canvas, &game, 1, 50),
        Err(RenderError::LevelNotInitialized)
    ));
    renderer.begin_level(&game).unwrap();
    renderer.draw(&mut canvas, &game, 1, 50).unwrap();
    let initial_screen = canvas.read_pixels(None, PixelFormatEnum::RGBA32).unwrap();
    for tick in 1..=40 {
        game.tick(Input {
            direction: Some(Direction::Right),
            action: false,
        });
        renderer.update_level(&game);
        if tick % 6 != 0 && tick != 40 {
            continue;
        }
        let saved = renderer.level.bitmap.pixels.clone();
        renderer.draw(&mut canvas, &game, 1, 50).unwrap();
        let displayed = canvas.read_pixels(None, PixelFormatEnum::RGBA32).unwrap();
        renderer.draw(&mut canvas, &game, 1, 50).unwrap();
        assert_eq!(
            canvas.read_pixels(None, PixelFormatEnum::RGBA32).unwrap(),
            displayed
        );
        assert_eq!(renderer.level.bitmap.pixels, saved);
        assert!(!renderer.level.bitmap.dirty);
        // Compare the actual SDL viewport to the saved bitmap after its
        // camera crop and integer scaling, including all opaque black.
        for y in 0..VIEW_HEIGHT as usize {
            for x in 0..LOGICAL_WIDTH as usize {
                let sx = (x + renderer.camera.x as usize) / MOVING_SCALE as usize;
                let sy = (y + renderer.camera.y as usize) / MOVING_SCALE as usize;
                let offset = (y * LOGICAL_WIDTH as usize + x) * 4;
                assert_eq!(&displayed[offset..offset + 4], pixel(&saved, sx, sy));
            }
        }
    }
    // Moving the viewport exposes the saved offscreen pixels; it does not
    // initialize a new board or replay actors at the camera's coordinates.
    let saved = renderer.level.bitmap.pixels.clone();
    canvas.set_draw_color(Color::RGB(0, 0, 0));
    canvas.clear();
    renderer
        .level
        .draw(&mut canvas, Camera { x: 64, y: 32 })
        .unwrap();
    let shifted = canvas.read_pixels(None, PixelFormatEnum::RGBA32).unwrap();
    assert_eq!(
        &shifted[..4],
        pixel(
            &saved,
            64 / MOVING_SCALE as usize,
            32 / MOVING_SCALE as usize
        )
    );
    assert_eq!(renderer.level.bitmap.pixels, saved);
    game.restart(&level).unwrap();
    renderer.begin_level(&game).unwrap();
    renderer.draw(&mut canvas, &game, 1, 50).unwrap();
    assert_eq!(
        canvas.read_pixels(None, PixelFormatEnum::RGBA32).unwrap(),
        initial_screen
    );
    // An Escape blast must be visible before another fixed update occurs.
    let tick = game.tick_count();
    game.destroy_murphy();
    renderer.update_level(&game);
    assert!(renderer.level.bitmap.dirty);
    renderer.draw(&mut canvas, &game, 1, 50).unwrap();
    assert_ne!(
        canvas.read_pixels(None, PixelFormatEnum::RGBA32).unwrap(),
        initial_screen
    );
    assert_eq!(game.tick_count(), tick);
}

/// Black is an opaque write into shared pixels, and later writes win.
#[test]
fn opaque_blits_preserve_order_and_pixels_outside_the_rectangle() {
    let mut saved = bitmap();
    saved
        .reset(&Board::new(2, 2, vec![State::empty(); 4]).unwrap())
        .unwrap();
    saved.pixels.pixels = [67, 123, 211, 255].repeat(32 * 32);
    // A falling rock's first two rows are original erase data. Their black
    // must replace even colored pixels belonging to another actor.
    let part = zonk_sprite_part(RoundedPhase::Falling(crate::actors::Frame::first())).unwrap();
    saved.copy(Position::new(0, 1), Blit::Moving(part));
    assert_eq!(pixel(&saved.pixels, 5, 0), [0, 0, 0, 255]);
    assert_eq!(pixel(&saved.pixels, 20, 0), [67, 123, 211, 255]);
    let after_rock = saved.pixels.clone();
    saved.copy(Position::new(0, 0), fixed_tile(3));
    assert_ne!(saved.pixels, after_rock);
    // Reversing overlapping writes deliberately produces a different image.
    saved.copy(Position::new(0, 1), Blit::Moving(part));
    assert_eq!(saved.pixels, after_rock);
    assert!(
        saved
            .pixels
            .pixels
            .as_chunks::<4>()
            .0
            .iter()
            .all(|p| p[3] == 255)
    );
}

/// Clipping keeps source and destination aligned at all four outer edges.
#[test]
fn clipping_advances_the_source_without_wrapping_rows() {
    let mut saved = bitmap();
    saved
        .reset(&Board::new(1, 1, vec![State::empty()]).unwrap())
        .unwrap();
    for (dx, dy) in [(-8, -7), (10, -7), (-8, 9), (10, 9)] {
        saved.pixels.pixels = [23, 31, 47, 255].repeat(16 * 16);
        let part = SpritePart {
            source: SourcePoint { x: 16, y: 0 },
            width: 16,
            height: 16,
            offset_x: dx,
            offset_y: dy,
        };
        saved.copy(Position::new(0, 0), Blit::Fixed(part));
        for y in 0..16 {
            for x in 0..16 {
                let sx = x as i32 - dx;
                let sy = y as i32 - dy;
                let expected = if (0..16).contains(&sx) && (0..16).contains(&sy) {
                    pixel(&saved.fixed, 16 + sx as usize, sy as usize)
                } else {
                    &[23, 31, 47, 255]
                };
                assert_eq!(pixel(&saved.pixels, x, y), expected);
            }
        }
    }
}

/// Reuse the two cell buffers across catch-up ticks and reset both on restart.
#[test]
fn frame_buffers_swap_without_reallocating_or_changing_gameplay() {
    for direction in [Direction::Left, Direction::Right] {
        let level = push_level(direction);
        let mut game = Game::with_random_seed(&level, 0).unwrap();
        let mut saved = bitmap();
        saved.reset(game.board()).unwrap();
        let initial = saved.pixels.clone();
        for _ in 0..40 {
            let frames = saved.frames.as_ref().unwrap();
            let old = frames.previous.clone();
            let old_pointer = frames.previous.cells().as_ptr();
            let scratch_pointer = frames.current.cells().as_ptr();
            game.tick(Input {
                direction: Some(direction),
                action: false,
            });
            let board = game.board().clone();
            saved.update(&game);
            let frames = saved.frames.as_ref().unwrap();
            assert_eq!(frames.previous, board);
            assert_eq!(frames.current, old);
            assert_eq!(frames.previous.cells().as_ptr(), scratch_pointer);
            assert_eq!(frames.current.cells().as_ptr(), old_pointer);
            assert_eq!(game.board(), &board);
        }
        game.restart(&level).unwrap();
        saved.reset(game.board()).unwrap();
        let frames = saved.frames.as_ref().unwrap();
        assert_eq!(frames.previous, frames.current);
        assert_eq!(saved.pixels, initial);
    }
}

/// Entering a collectible preserves material until the original strip erases it.
#[test]
fn eating_base_does_not_repaint_consumed_material_between_frames() {
    let mut record = vec![0; LEVEL_RECORD_SIZE];
    record[..60 * 24].fill(6);
    record[2 * LEVEL_WIDTH + 2] = 3;
    record[2 * LEVEL_WIDTH + 3] = 2;
    let level = LevelSet::new(&record).load(1).unwrap();
    let mut game = Game::with_random_seed(&level, 0).unwrap();
    let mut saved = bitmap();
    saved.reset(game.board()).unwrap();
    for _ in 0..8 {
        game.tick(Input {
            direction: Some(Direction::Right),
            action: false,
        });
        saved.update(&game);
    }
    // The old target can no longer appear behind freshly alpha-masked
    // pictures: these pixels now come from the resolved Murphy frames.
    let occupied = Position::new(3, 2);
    let Actor::Murphy(murphy) = game.board().state(occupied).unwrap().actor() else {
        unreachable!()
    };
    let copies = actor_sprites(&Actor::Murphy(*murphy))
        .iter()
        .collect::<Vec<_>>();
    assert!(!copies.is_empty());
    let mut expected = bitmap();
    expected
        .reset(&Board::new(60, 24, vec![State::empty(); 60 * 24]).unwrap())
        .unwrap();
    for copy in copies {
        expected.copy(occupied, copy);
    }
    for y in 32..48 {
        for x in 48..64 {
            assert_eq!(pixel(&saved.pixels, x, y), pixel(&expected.pixels, x, y));
        }
    }
}

/// Previous/current actions retain the last push, port, and Exit picture on removal.
#[test]
fn completed_actions_resolve_every_picture_and_erase_their_source() {
    for direction in Direction::ALL {
        for tile in [1, 8, 18, 7, 23] {
            if matches!(tile, 1 | 8) && !direction.is_horizontal() {
                continue;
            }
            let origin = Position::new(4, 4);
            let (target, destination) = match direction {
                Direction::Up => (Position::new(4, 3), Position::new(4, 2)),
                Direction::Down => (Position::new(4, 5), Position::new(4, 6)),
                Direction::Left => (Position::new(3, 4), Position::new(2, 4)),
                Direction::Right => (Position::new(5, 4), Position::new(6, 4)),
            };
            let mut record = vec![0; LEVEL_RECORD_SIZE];
            record[..60 * 24].fill(6);
            record[origin.y * LEVEL_WIDTH + origin.x] = 3;
            record[target.y * LEVEL_WIDTH + target.x] = tile;
            record[destination.y * LEVEL_WIDTH + destination.x] = 0;
            let level = LevelSet::new(&record).load(1).unwrap();
            let mut game = Game::with_random_seed(&level, 0).unwrap();
            let mut saved = bitmap();
            saved.reset(game.board()).unwrap();
            let (ticks, length) = match tile {
                7 => (40, 40),
                23 => (8, 8),
                _ => (16, 8),
            };
            let mut rendered = 0;
            for tick in 0..ticks {
                game.tick(Input {
                    direction: Some(direction),
                    action: false,
                });
                let frames = saved.frames.as_ref().unwrap();
                if (matches!(tile, 7 | 23) || tick >= 8)
                    && let Some((anchor, parts)) = murphy_sprites(&frames.previous, game.board())
                {
                    assert_eq!(anchor, origin, "completion keeps the original source");
                    let count = parts.iter().count();
                    assert_eq!(count, if tile == 23 { 2 } else { 1 });
                    rendered += 1;
                }
                saved.update(&game);
            }
            assert_eq!(
                rendered, length,
                "tile {tile}, {direction:?}: complete strip"
            );
            assert!(game.board().state(origin).unwrap().is_empty());
            for y in 0..16 {
                for x in 0..16 {
                    assert_eq!(
                        pixel(&saved.pixels, origin.x * 16 + x, origin.y * 16 + y),
                        [0, 0, 0, 255],
                        "tile {tile}, {direction:?}: source remnant at ({x}, {y})"
                    );
                }
            }
        }
    }
}

/// Preparation reserves the held rock without repainting it over Murphy's strip.
#[test]
fn preparation_preserves_the_held_target() {
    for direction in [Direction::Left, Direction::Right] {
        let level = push_level(direction);
        let mut game = Game::with_random_seed(&level, 0).unwrap();
        let mut saved = bitmap();
        saved.reset(game.board()).unwrap();
        for _ in 0..8 {
            game.tick(Input {
                direction: Some(direction),
                action: false,
            });
            saved.update(&game);
        }
        for y in 0..16 {
            for x in 0..16 {
                assert_eq!(
                    pixel(&saved.pixels, 4 * 16 + x, 2 * 16 + y),
                    pixel(&saved.fixed, 16 + x, y)
                );
            }
        }
        // Later source releases change occupancy without a separate Space sprite.
        for _ in 0..32 {
            game.tick(Input {
                direction: Some(direction),
                action: false,
            });
            saved.update(&game);
        }
    }
}

/// Match every upstream bitmap while following rocks and revisiting cleared Base.
#[test]
fn rounded_objects_and_cleared_base_match_opensupaplex_every_tick() {
    for (reference, returning) in [
        (
            include_str!("../../../tests/support/opensupaplex_push_trace.txt"),
            false,
        ),
        (
            include_str!("../../../tests/support/opensupaplex_follow_trace.txt"),
            true,
        ),
    ] {
        for case in reference.split("CASE ").skip(1) {
            let (description, trace) = case.split_once('\n').unwrap();
            let (tile, direction_name) = match description.split_once(' ') {
                Some((tile, direction)) => (tile.parse::<u8>().unwrap(), direction),
                None => (1, description),
            };
            let direction = match direction_name {
                "left" => Direction::Left,
                "right" => Direction::Right,
                _ => panic!("invalid case"),
            };
            let level = if returning {
                let mut record = vec![0; LEVEL_RECORD_SIZE];
                record[..60 * 24].fill(6);
                for y in 1..7 {
                    for x in 1..9 {
                        record[y * LEVEL_WIDTH + x] = 0;
                    }
                }
                let (start, object) = match direction {
                    Direction::Left => (5, 3),
                    _ => (3, 5),
                };
                record[2 * LEVEL_WIDTH + start] = 3;
                record[2 * LEVEL_WIDTH + 4] = 2;
                record[2 * LEVEL_WIDTH + object] = tile;
                record[3 * LEVEL_WIDTH + 4] = 6;
                record[3 * LEVEL_WIDTH + object] = 5;
                LevelSet::new(&record).load(1).unwrap()
            } else {
                push_level(direction)
            };
            let mut game = Game::with_random_seed(&level, 0).unwrap();
            let mut actual = bitmap();
            actual.reset(game.board()).unwrap();
            let mut expected = bitmap();
            expected.reset(game.board()).unwrap();
            for line in trace.lines().filter(|l| !l.is_empty()) {
                let fields = line.split_whitespace().collect::<Vec<_>>();
                match fields[0] {
                    "BLIT" => {
                        let values = fields[2..]
                            .iter()
                            .map(|v| v.parse::<i32>().unwrap())
                            .collect::<Vec<_>>();
                        let [sx, sy, width, height, dx, dy] = values[..] else {
                            panic!("invalid blit");
                        };
                        expected.copy(
                            Position::new(0, 0),
                            Blit::Moving(SpritePart {
                                source: SourcePoint { x: sx, y: sy },
                                width: width as u32,
                                height: height as u32,
                                offset_x: dx,
                                offset_y: dy,
                            }),
                        );
                    }
                    "STATE" => {
                        let tick = fields[1].parse::<u8>().unwrap();
                        let direction = if returning && (17..=24).contains(&tick) {
                            direction.opposite()
                        } else {
                            direction
                        };
                        game.tick(Input {
                            direction: Some(direction),
                            action: false,
                        });
                        actual.update(&game);
                        assert_eq!(
                            game.murphy_position(),
                            Some(Position::new(
                                fields[3].parse().unwrap(),
                                fields[4].parse().unwrap()
                            )),
                            "{description}, tick {tick}: Murphy"
                        );
                        let difference = actual
                            .pixels
                            .pixels
                            .iter()
                            .zip(&expected.pixels.pixels)
                            .position(|(a, b)| a != b);
                        assert!(
                            difference.is_none(),
                            "{description}, tick {tick}: first different pixel {:?}",
                            difference.map(|i| (i / 4 % 960, i / 4 / 960))
                        );
                    }
                    _ => panic!("invalid reference entry"),
                }
            }
        }
    }
}

/// Eating Base and reversing through cleared cells must match every upstream frame.
#[test]
fn walking_back_through_eaten_base_matches_opensupaplex_pixels() {
    let reference = include_str!("../../../tests/support/opensupaplex_walk_trace.txt");
    for case in reference.split("CASE ").skip(1) {
        let (description, trace) = case.split_once('\n').unwrap();
        let (direction, target) = match description {
            "up" => (Direction::Up, Position::new(4, 2)),
            "left" => (Direction::Left, Position::new(3, 3)),
            "down" => (Direction::Down, Position::new(4, 4)),
            "right" => (Direction::Right, Position::new(5, 3)),
            _ => panic!("unknown reference direction"),
        };
        let mut record = vec![0; LEVEL_RECORD_SIZE];
        record[..60 * 24].fill(6);
        record[3 * LEVEL_WIDTH + 4] = 3;
        record[target.y * LEVEL_WIDTH + target.x] = 2;
        let level = LevelSet::new(&record).load(1).unwrap();
        let mut game = Game::with_random_seed(&level, 0).unwrap();
        let mut actual = bitmap();
        actual.reset(game.board()).unwrap();
        let mut expected = bitmap();
        expected.reset(game.board()).unwrap();
        for line in trace.lines().filter(|line| !line.is_empty()) {
            let (kind, values) = line.split_once(' ').unwrap();
            let values = values
                .split_whitespace()
                .map(|v| v.parse::<i32>().unwrap())
                .collect::<Vec<_>>();
            match kind {
                "BLIT" => {
                    let [_, sx, sy, width, height, dx, dy] = values[..] else {
                        panic!("invalid upstream copy");
                    };
                    expected.copy(
                        Position::new(0, 0),
                        Blit::Moving(SpritePart {
                            source: SourcePoint { x: sx, y: sy },
                            width: width as u32,
                            height: height as u32,
                            offset_x: dx,
                            offset_y: dy,
                        }),
                    );
                }
                "STATE" => {
                    let tick = values[0];
                    let direction = if (9..=16).contains(&tick) {
                        direction.opposite()
                    } else {
                        direction
                    };
                    game.tick(Input {
                        direction: Some(direction),
                        action: false,
                    });
                    actual.update(&game);
                    let position = game.murphy_position().unwrap();
                    assert_eq!(
                        (position.y * LEVEL_WIDTH + position.x) as i32,
                        values[1],
                        "{description}, tick {tick}"
                    );
                    let difference = actual
                        .pixels
                        .pixels
                        .iter()
                        .zip(&expected.pixels.pixels)
                        .position(|(a, b)| a != b);
                    assert!(
                        difference.is_none(),
                        "{description}, tick {tick}: first different pixel {:?}",
                        difference.map(|i| (i / 4 % 960, i / 4 / 960))
                    );
                }
                _ => panic!("unknown reference entry"),
            }
        }
    }
}

/// Compare actual emitted copies and completion timing with an independent C trace.
#[test]
fn snap_cell_pairs_match_opensupaplex() {
    let reference = include_str!("../../../tests/support/opensupaplex_snap_trace.txt");
    for case in reference.split("CASE ").skip(1) {
        let (description, expected) = case.split_once('\n').unwrap();
        let (tile, direction) = description.split_once(' ').unwrap();
        let tile = tile.parse::<u8>().unwrap();
        let (direction, target) = match direction {
            "up" => (Direction::Up, Position::new(4, 2)),
            "left" => (Direction::Left, Position::new(3, 3)),
            "down" => (Direction::Down, Position::new(4, 4)),
            "right" => (Direction::Right, Position::new(5, 3)),
            _ => panic!("unknown reference direction"),
        };
        // Match the upstream harness: all hardware except Murphy (4,3) and
        // one adjacent collectible. No other actor can affect the trace.
        let mut record = vec![0; LEVEL_RECORD_SIZE];
        record[..60 * 24].fill(6);
        record[3 * LEVEL_WIDTH + 4] = 3;
        record[target.y * LEVEL_WIDTH + target.x] = tile;
        let level = LevelSet::new(&record).load(1).unwrap();
        let mut game = Game::with_random_seed(&level, 0).unwrap();
        let mut actual = Vec::new();
        let mut previous = game.board().clone();
        for tick in 1..=8 {
            game.tick(Input {
                direction: Some(direction),
                action: true,
            });
            for (position, copy) in sprites_in_frame(&previous, game.board(), game.freeze_zonks()) {
                let Blit::Moving(part) = copy else {
                    panic!("unexpected cleanup for {description}");
                };
                actual.push(format!(
                    "BLIT {tick} {} {} {} {} {} {}",
                    part.source.x,
                    part.source.y,
                    part.width,
                    part.height,
                    position.x as i32 * 16 + part.offset_x,
                    position.y as i32 * 16 + part.offset_y
                ));
            }
            previous.copy_from(game.board());
            actual.push(format!(
                "STATE {tick} {} {}",
                game.board().state(target).unwrap().actor().tile_code(),
                game.remaining_infotrons(),
            ));
        }
        assert_eq!(actual.join("\n"), expected.trim(), "{description}");
    }
}

/// A completed adjacent collection must leave its entire target cell black.
#[test]
fn every_snap_finishes_with_no_target_pixels() {
    let mut failures = Vec::new();
    for tile in [4, 2, 20] {
        for direction in Direction::ALL {
            let mut record = vec![0; LEVEL_RECORD_SIZE];
            record[..60 * 24].fill(6);
            let origin = Position::new(4, 3);
            let target = match direction {
                Direction::Up => Position::new(4, 2),
                Direction::Down => Position::new(4, 4),
                Direction::Left => Position::new(3, 3),
                Direction::Right => Position::new(5, 3),
            };
            record[origin.y * LEVEL_WIDTH + origin.x] = 3;
            record[target.y * LEVEL_WIDTH + target.x] = tile;
            let level = LevelSet::new(&record).load(1).unwrap();
            let mut game = Game::with_random_seed(&level, 0).unwrap();
            let mut saved = bitmap();
            saved.reset(game.board()).unwrap();
            for _ in 0..12 {
                game.tick(Input {
                    direction: Some(direction),
                    action: true,
                });
                saved.update(&game);
            }
            assert!(game.board().state(target).unwrap().is_empty());
            let mut remaining = Vec::new();
            for y in 0..16 {
                for x in 0..16 {
                    if pixel(&saved.pixels, target.x * 16 + x, target.y * 16 + y) != [0, 0, 0, 255]
                    {
                        remaining.push((x, y));
                    }
                }
            }
            if !remaining.is_empty() {
                failures.push(format!(
                    "tile {tile}, {direction:?}: remaining {remaining:?}"
                ));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// Submitting an already resolved board preserves both allocations and displayed pixels.
#[test]
fn repeated_frame_submission_does_not_swap_or_advance_pictures() {
    let level = push_level(Direction::Right);
    let mut game = Game::with_random_seed(&level, 0).unwrap();
    let mut saved = bitmap();
    saved.reset(game.board()).unwrap();
    for _ in 0..40 {
        game.tick(Input {
            direction: Some(Direction::Right),
            action: false,
        });
        saved.update(&game);
        let pixels = saved.pixels.clone();
        let frames = saved.frames.as_ref().unwrap();
        let previous = frames.previous.cells().as_ptr();
        let current = frames.current.cells().as_ptr();
        // Model the texture upload: a repeated submission must not dirty it again.
        saved.dirty = false;
        saved.update(&game);
        let frames = saved.frames.as_ref().unwrap();
        assert_eq!(frames.previous.cells().as_ptr(), previous);
        assert_eq!(frames.current.cells().as_ptr(), current);
        assert_eq!(saved.pixels, pixels);
        assert!(!saved.dirty);
    }
}

/// An Escape blast between ticks changes its cells without consuming distant fall frames.
#[test]
fn immediate_death_preserves_unaffected_actors_at_the_same_tick() {
    let mut record = vec![0; LEVEL_RECORD_SIZE];
    record[..60 * 24].fill(6);
    record[2 * LEVEL_WIDTH + 2] = 3;
    record[2 * LEVEL_WIDTH + 7] = 1;
    for y in 3..8 {
        record[y * LEVEL_WIDTH + 7] = 0;
    }
    let level = LevelSet::new(&record).load(1).unwrap();
    // Include the pending first picture and every remaining in-flight picture.
    for steps in 2..10 {
        let mut game = Game::with_random_seed(&level, 0).unwrap();
        let mut saved = bitmap();
        saved.reset(game.board()).unwrap();
        for _ in 0..steps {
            game.tick(Input::default());
            saved.update(&game);
        }
        let before = saved.pixels.clone();
        let tick = game.tick_count();
        game.destroy_murphy();
        saved.update(&game);
        assert_eq!(game.tick_count(), tick);
        assert_ne!(saved.pixels, before, "the immediate blast must be visible");
        assert_eq!(&saved.frames.as_ref().unwrap().previous, game.board());
        for y in 0..before.height as usize {
            for x in 7 * 16..8 * 16 {
                assert_eq!(
                    pixel(&saved.pixels, x, y),
                    pixel(&before, x, y),
                    "death at tick {steps} advanced the distant rock at ({x}, {y})"
                );
            }
        }
    }
}

/// A Terminal's single cached sprite exactly retains the original scanline scroll.
#[test]
fn terminal_cache_matches_every_original_scroll_phase() {
    let saved = bitmap();
    let mut terminal = crate::actors::Terminal::new();
    for phase in 0..7 {
        let parts = actor_sprites(&Actor::Terminal(terminal));
        let sprites = parts.iter().collect::<Vec<_>>();
        assert_eq!(sprites.len(), 1);
        let Blit::Terminal(part) = sprites[0] else {
            panic!("a Terminal must resolve to its complete cached tile");
        };
        assert_eq!(part.width, 16);
        assert_eq!(part.height, 16);
        assert_eq!(part.source.x, phase * 16);
        for y in 0..16 {
            // Original scrolls cycle seven pattern rows and repeat the top row
            // in the eighth screen line. The initial tile uses its original row 2.
            let rows = [
                [2, 3, 4, 5, 6, 7, 8, 9],
                [3, 4, 5, 6, 7, 8, 9, 3],
                [4, 5, 6, 7, 8, 9, 3, 4],
                [5, 6, 7, 8, 9, 3, 4, 5],
                [6, 7, 8, 9, 3, 4, 5, 6],
                [7, 8, 9, 3, 4, 5, 6, 7],
                [8, 9, 3, 4, 5, 6, 7, 8],
            ];
            let source_y = if (2..=9).contains(&y) {
                rows[phase as usize][y - 2]
            } else {
                y
            };
            for x in 0..16 {
                assert_eq!(
                    pixel(&saved.terminal, phase as usize * 16 + x, y),
                    pixel(&saved.fixed, 19 * 16 + x, source_y),
                    "phase {phase}, ({x}, {y})"
                );
            }
        }
        terminal = terminal.after_scroll(-1);
    }
}
