//! Persistent level pixels, changed only by simulation-ordered opaque copies.
//!
//! OpenSupaplex's `drawMovingSpriteFrameInLevel` copies directly into its saved
//! level bitmap. Black bytes erase earlier drawings, including other actors.
//! A display refresh only views those pixels; it never reconstructs the board.

use super::{
    BlackPixelPolicy, Camera, DecodedPng, MOVING_SCALE, RenderError, SourcePoint, SpritePart,
    apply_black_pixel_policy, bug_sprite_part, decode_sized_png, electron_sprite_part,
    explosion_sprite_part, fixed_tile_source, infotron_sprite_part, orange_sprite_part,
    snik_snak_sprite_part, sprite_parts, terminal_source_row, upload_texture, zonk_sprite_part,
};
use crate::{
    actors::{Actor, Bug, Empty, Position, orange_disk::OrangePhase},
    assets::{FIXED_GRAPHICS_PATH, MOVING_GRAPHICS_PATH},
    game::{Board, BoardChange},
    platform::{
        rect::Rect,
        render::{BlendMode, Canvas, Texture, TextureCreator},
        video::{Window, WindowContext},
    },
};

/// A single opaque atlas copy; ownership, collision, and scheduling stay in Game.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Blit {
    /// Original fixed artwork, including individual Terminal screen scanlines.
    Fixed(SpritePart),
    /// Original animation rectangle with every black erase pixel intact.
    Moving(SpritePart),
}

/// CPU-side artwork and saved board pixels, independent of display refreshes.
struct LevelBitmap {
    /// Original 640×16 fixed atlas, decoded once per renderer.
    fixed: DecodedPng,
    /// Original 320×462 moving atlas, decoded without a color key.
    moving: DecodedPng,
    /// Entire board at original 16-pixel tile resolution, including offscreen cells.
    pixels: DecodedPng,
    /// Whether committed copies require a texture upload at the next display.
    dirty: bool,
}

impl LevelBitmap {
    /// Decodes immutable atlases; the first level supplies the bitmap dimensions.
    fn new(fixed: &[u8], moving: &[u8]) -> Result<Self, RenderError> {
        let mut fixed = decode_sized_png(fixed, 640, 16, FIXED_GRAPHICS_PATH)?;
        let mut moving = decode_sized_png(moving, 320, 462, MOVING_GRAPHICS_PATH)?;
        // Original bitmaps have no alpha channel. Preserve that contract even
        // when an unbundled replacement PNG contains transparent black bytes.
        apply_black_pixel_policy(&mut fixed.pixels, BlackPixelPolicy::Opaque);
        apply_black_pixel_policy(&mut moving.pixels, BlackPixelPolicy::Opaque);
        Ok(Self {
            fixed,
            moving,
            pixels: DecodedPng {
                width: 0,
                height: 0,
                pixels: Vec::new(),
            },
            dirty: false,
        })
    }

    /// Starts a fresh level image; restarting discards all previous drawing history.
    fn reset(&mut self, board: &Board) -> Result<(), RenderError> {
        // Check dimensions before allocation and before signed sprite arithmetic.
        let width = board
            .width()
            .checked_mul(16)
            .ok_or(RenderError::ImageTooLarge)?;
        let height = board
            .height()
            .checked_mul(16)
            .ok_or(RenderError::ImageTooLarge)?;
        let size = width
            .checked_mul(height)
            .and_then(|area| area.checked_mul(4))
            .ok_or(RenderError::ImageTooLarge)?;
        if width == 0 || height == 0 || width > i32::MAX as usize || height > i32::MAX as usize {
            return Err(RenderError::ImageTooLarge);
        }
        self.pixels = DecodedPng {
            width: width as u32,
            height: height as u32,
            pixels: [0, 0, 0, 255].repeat(size / 4),
        };
        // Seed terrain once, then loaded moving enemies. Subsequent simulation
        // changes never use these initialization passes as their draw schedule.
        for (index, state) in board.cells().iter().enumerate() {
            let position = board.position(index).expect("enumerated cell is in bounds");
            let tile = match state.actor() {
                Actor::Empty(_) | Actor::InvisibleWall(_) => 0,
                _ => state.actor().tile_code(),
            };
            self.copy(position, fixed_tile(tile));
        }
        for (index, state) in board.cells().iter().enumerate() {
            let position = board.position(index).expect("enumerated cell is in bounds");
            for blit in artwork(state.actor()) {
                self.copy(position, blit);
            }
        }
        self.dirty = true;
        Ok(())
    }

    /// Applies each committed change immediately, retaining all intervening copies.
    fn apply(&mut self, changes: &[BoardChange]) {
        for change in changes {
            match (change.before.actor(), change.after.actor()) {
                // A reservation release changes collision data only. Repainting
                // Space here could erase Murphy after he has entered the cell.
                (Actor::Explosion(_), Actor::Empty(Empty::Space)) => {
                    self.copy(change.position, fixed_tile(0));
                }
                (_, Actor::Empty(_) | Actor::InvisibleWall(_)) => {}
                (before, after) => {
                    let next = artwork(after);
                    if next == artwork(before) {
                        // Fuse counters, held flags, and randomized waits can
                        // change without causing a new drawing in the bitmap.
                        continue;
                    }
                    if let Actor::Murphy(murphy) = after
                        && let Some((action, frame)) = murphy.sprite_pose()
                        && let Some(retained) = sprite_parts(action, frame).retained
                    {
                        // The original snap handler paints this pose once at
                        // entry. Red Disk snapping keeps the already saved pose.
                        self.copy(change.position, Blit::Moving(retained));
                    }
                    for blit in next {
                        self.copy(change.position, blit);
                    }
                }
            }
        }
    }

    /// Copies one full opaque rectangle, clipped only at the level's outer edge.
    fn copy(&mut self, position: Position, blit: Blit) {
        let (atlas, part) = match blit {
            Blit::Fixed(part) => (&self.fixed, part),
            Blit::Moving(part) => (&self.moving, part),
        };
        // Clip in signed coordinates before converting to byte offsets. Actors
        // can straddle custom board edges; clipping must advance the source too.
        let x = position.x as i64 * 16 + i64::from(part.offset_x);
        let y = position.y as i64 * 16 + i64::from(part.offset_y);
        let left = x.max(0);
        let top = y.max(0);
        let right = (x + i64::from(part.width)).min(i64::from(self.pixels.width));
        let bottom = (y + i64::from(part.height)).min(i64::from(self.pixels.height));
        if left >= right || top >= bottom {
            return;
        }
        let bytes = (right - left) as usize * 4;
        for row in top..bottom {
            let source = ((i64::from(part.source.y) + row - y) as usize * atlas.width as usize
                + (i64::from(part.source.x) + left - x) as usize)
                * 4;
            let destination = (row as usize * self.pixels.width as usize + left as usize) * 4;
            self.pixels.pixels[destination..destination + bytes]
                .copy_from_slice(&atlas.pixels[source..source + bytes]);
        }
        self.dirty = true;
    }
}

/// Selects only this actor's current copies, never neighboring background layers.
fn artwork(actor: &Actor) -> Vec<Blit> {
    let part = match actor {
        Actor::Empty(_) | Actor::InvisibleWall(_) => return Vec::new(),
        Actor::Zonk(actor) => zonk_sprite_part(actor.phase()),
        Actor::Infotron(actor) => infotron_sprite_part(actor.phase()),
        Actor::OrangeDisk(actor) => match actor.phase() {
            OrangePhase::Falling(frame) => Some(orange_sprite_part(frame)),
            OrangePhase::Resting
            | OrangePhase::AwaitingFall(_)
            | OrangePhase::Fuse(_)
            | OrangePhase::Held => None,
        },
        Actor::Murphy(actor) => {
            if let Some((action, frame)) = actor.sprite_pose() {
                let parts = sprite_parts(action, frame);
                // Target material and the retained snap pose are already in
                // the level bitmap. Repainting them would undo earlier erases.
                return std::iter::once(Blit::Moving(parts.primary))
                    .chain(parts.secondary.map(Blit::Moving))
                    .collect();
            }
            None
        }
        Actor::SnikSnak(actor) => Some(snik_snak_sprite_part(actor.phase())),
        Actor::Electron(actor) => Some(electron_sprite_part(actor.phase())),
        Actor::Explosion(actor) => Some(explosion_sprite_part(actor.residue(), actor.frame())),
        Actor::Bug(Bug::Active(frame)) => Some(bug_sprite_part(*frame)),
        Actor::Bug(Bug::Dormant(_)) => return vec![fixed_tile(2)],
        Actor::Terminal(actor) => {
            let mut copies = vec![fixed_tile(19)];
            for row in 2..=9 {
                copies.push(Blit::Fixed(SpritePart {
                    source: SourcePoint {
                        x: 19 * 16,
                        y: i32::from(terminal_source_row(actor.screen_frame(), row)),
                    },
                    width: 16,
                    height: 1,
                    offset_x: 0,
                    offset_y: i32::from(row),
                }));
            }
            return copies;
        }
        Actor::Base(_)
        | Actor::RamChip(_)
        | Actor::Hardware(_)
        | Actor::Exit(_)
        | Actor::Port(_)
        | Actor::YellowDisk(_)
        | Actor::RedDisk(_)
        | Actor::Bug(Bug::Held) => None,
    };
    vec![match part {
        Some(part) => Blit::Moving(part),
        None => fixed_tile(actor.tile_code()),
    }]
}

/// Describes a complete fixed tile at its unscaled board coordinate.
fn fixed_tile(tile: u8) -> Blit {
    let source = fixed_tile_source(tile);
    Blit::Fixed(SpritePart {
        source: SourcePoint {
            x: source.x(),
            y: source.y(),
        },
        width: 16,
        height: 16,
        offset_x: 0,
        offset_y: 0,
    })
}

/// Uploads the saved level image without replaying actor drawings on display ticks.
pub(super) struct LevelRenderer<'textures> {
    /// SDL context used when a new level needs a streaming texture.
    creator: &'textures TextureCreator<WindowContext>,
    /// CPU image retaining the order of all simulation copies, even offscreen.
    bitmap: LevelBitmap,
    /// Absent in menus before the first level is explicitly initialized.
    texture: Option<Texture<'textures>>,
}

impl<'textures> LevelRenderer<'textures> {
    /// Loads atlases without allocating a board image for menu-only sessions.
    pub(super) fn new(
        creator: &'textures TextureCreator<WindowContext>,
        fixed: &[u8],
        moving: &[u8],
    ) -> Result<Self, RenderError> {
        Ok(Self {
            creator,
            bitmap: LevelBitmap::new(fixed, moving)?,
            texture: None,
        })
    }

    /// Replaces both bitmap and texture at each level start or restart.
    pub(super) fn reset(&mut self, board: &Board) -> Result<(), RenderError> {
        self.bitmap.reset(board)?;
        self.texture = Some(upload_texture(
            self.creator,
            &self.bitmap.pixels,
            BlendMode::None,
        )?);
        self.bitmap.dirty = false;
        Ok(())
    }

    /// Consumes a simulation batch before another tick may advance the game.
    pub(super) fn apply(&mut self, changes: &[BoardChange]) {
        assert!(
            self.texture.is_some(),
            "begin_level must precede gameplay updates"
        );
        self.bitmap.apply(changes);
    }

    /// Presents the saved pixels at the camera offset; no actors are visited here.
    pub(super) fn draw(
        &mut self,
        canvas: &mut Canvas<Window>,
        camera: Camera,
    ) -> Result<(), RenderError> {
        let texture = self
            .texture
            .as_mut()
            .ok_or(RenderError::LevelNotInitialized)?;
        let image = &self.bitmap.pixels;
        if self.bitmap.dirty {
            // Several catch-up ticks share one upload, but every intervening
            // opaque copy has already modified the CPU image in tick order.
            texture
                .update(None, &image.pixels, image.width as usize * 4)
                .map_err(|error| RenderError::Sdl(error.to_string()))?;
            self.bitmap.dirty = false;
        }
        canvas
            .copy(
                texture,
                None,
                Rect::new(
                    -camera.x,
                    -camera.y,
                    image.width * MOVING_SCALE,
                    image.height * MOVING_SCALE,
                ),
            )
            .map_err(RenderError::Sdl)
    }
}

#[cfg(test)]
mod tests {
    //! Test saved pixels and ordered writes independently of SDL frame cadence.

    use super::*;
    use crate::{
        actors::{Direction, State, Zonk, rounded::RoundedPhase},
        assets,
        game::{Game, Input},
        level::{LEVEL_RECORD_SIZE, LEVEL_WIDTH, Level, LevelSet},
    };

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
            renderer.apply_board_changes(&game.tick_with_changes(Input {
                direction: Some(Direction::Right),
                action: false,
            }));
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
        renderer.apply_board_changes(&game.destroy_murphy_with_changes());
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

    /// A collision-marker release cannot erase an actor already drawn there.
    #[test]
    fn releasing_a_source_retains_its_saved_pixels() {
        let mut saved = bitmap();
        saved
            .reset(&Board::new(1, 1, vec![State::empty()]).unwrap())
            .unwrap();
        saved.copy(Position::new(0, 0), fixed_tile(3));
        let before = saved.pixels.clone();
        saved.dirty = false;
        saved.apply(&[BoardChange {
            position: Position::new(0, 0),
            before: State::new(Actor::Empty(Empty::Reserved(
                crate::actors::empty::Reservation::Vacating {
                    direction: Direction::Down,
                    duration: crate::actors::empty::SourceDuration::Eight,
                },
            ))),
            after: State::empty(),
        }]);
        assert_eq!(saved.pixels, before);
        assert!(!saved.dirty);
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

    /// Both push directions use Murphy's update before the overlapping rock copy.
    #[test]
    fn push_roll_and_fall_follow_simulation_order_at_every_display_cadence() {
        for direction in [Direction::Left, Direction::Right] {
            let level = push_level(direction);
            let mut game = Game::with_random_seed(&level, 0).unwrap();
            let mut saved = bitmap();
            saved.reset(game.board()).unwrap();
            let initial = saved.pixels.clone();
            let mut delayed = bitmap();
            delayed.reset(game.board()).unwrap();
            let mut pending = Vec::new();
            let mut overlaps = 0;
            for tick in 1..=40 {
                let changes = game.tick_with_changes(Input {
                    direction: Some(direction),
                    action: false,
                });
                saved.apply(&changes);
                pending.extend(changes.iter().cloned());
                // A final board scan would order a left-side rock before Murphy.
                // The committed stream always retains Murphy's callback first.
                let player = changes
                    .iter()
                    .position(|c| matches!(c.after.actor(), Actor::Murphy(_)));
                let rock = changes.iter().rposition(|c| matches!(c.after.actor(), Actor::Zonk(z) if matches!(z.phase(), RoundedPhase::Rolling { .. } | RoundedPhase::Falling(_))));
                if let (Some(player), Some(rock)) = (player, rock) {
                    assert!(player < rock, "{direction:?} tick {tick}");
                    let change = &changes[rock];
                    let Actor::Zonk(zonk) = change.after.actor() else {
                        unreachable!()
                    };
                    let part = zonk_sprite_part(zonk.phase()).unwrap();
                    let x = change.position.x as i32 * 16 + part.offset_x;
                    let y = change.position.y as i32 * 16 + part.offset_y;
                    // Verify the *entire* original rectangle, including black.
                    // Zero-overlap assertions would disagree with upstream's
                    // opaque memcpy: later Zonk erase bytes can cover Murphy.
                    for dy in 0..part.height as usize {
                        for dx in 0..part.width as usize {
                            assert_eq!(
                                pixel(&saved.pixels, x as usize + dx, y as usize + dy),
                                pixel(
                                    &saved.moving,
                                    part.source.x as usize + dx,
                                    part.source.y as usize + dy
                                ),
                                "{direction:?} tick {tick} rectangle ({dx}, {dy})"
                            );
                        }
                    }
                    overlaps += 1;
                }
                // Keep the existing legal-follow windows; this renderer must
                // not change collision timing to make its pixel test pass.
                if tick == 18 {
                    assert_eq!(
                        game.murphy_position(),
                        Some(Position::new(
                            if direction == Direction::Left { 3 } else { 5 },
                            2
                        ))
                    );
                }
                if tick == 26 {
                    assert_eq!(
                        game.murphy_position(),
                        Some(Position::new(
                            if direction == Direction::Left { 2 } else { 6 },
                            2
                        ))
                    );
                }
                if tick % 6 == 0 || tick == 40 {
                    delayed.apply(&pending);
                    pending.clear();
                    assert_eq!(delayed.pixels, saved.pixels);
                }
            }
            assert!(overlaps >= 15, "exercise both the roll and fall");
            game.restart(&level).unwrap();
            saved.reset(game.board()).unwrap();
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
        for _ in 0..10 {
            saved.apply(&game.tick_with_changes(Input {
                direction: Some(Direction::Right),
                action: false,
            }));
        }
        // The old target can no longer appear behind freshly alpha-masked
        // pictures: all pixels now come from opaque Murphy drawing history.
        let occupied = Position::new(3, 2);
        let Actor::Murphy(murphy) = game.board().state(occupied).unwrap().actor() else {
            unreachable!()
        };
        let copies = artwork(&Actor::Murphy(*murphy));
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

    /// Nonvisual state changes do not restore a fixed tile over later drawings.
    #[test]
    fn a_held_rock_keeps_its_previous_artwork_without_repainting() {
        let mut saved = bitmap();
        saved
            .reset(&Board::new(1, 1, vec![State::new(Actor::Zonk(Zonk::resting()))]).unwrap())
            .unwrap();
        saved.pixels.pixels = [17, 31, 47, 255].repeat(16 * 16);
        saved.dirty = false;
        saved.apply(&[BoardChange {
            position: Position::new(0, 0),
            before: State::new(Actor::Zonk(Zonk::resting())),
            after: State::new(Actor::Zonk(Zonk::from_phase(RoundedPhase::Held))),
        }]);
        assert!(!saved.dirty);
        assert!(
            saved
                .pixels
                .pixels
                .as_chunks::<4>()
                .0
                .iter()
                .all(|p| *p == [17, 31, 47, 255])
        );
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
                    saved.apply(&game.tick_with_changes(Input {
                        direction: Some(direction),
                        action: true,
                    }));
                }
                assert!(game.board().state(target).unwrap().is_empty());
                let mut remaining = Vec::new();
                for y in 0..16 {
                    for x in 0..16 {
                        if pixel(&saved.pixels, target.x * 16 + x, target.y * 16 + y)
                            != [0, 0, 0, 255]
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
}
