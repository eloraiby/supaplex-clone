//! Previous/current cell buffers resolved into persistent, opaque level pixels.
//!
//! OpenSupaplex's `drawMovingSpriteFrameInLevel` copies directly into its saved
//! level bitmap. Black bytes erase earlier drawings, including other actors.
//! A display refresh only views those pixels; it never reconstructs the board.

use super::cell::{Sprite as Blit, actor_sprites, cell_sprites, fixed_tile, murphy_sprites};
use super::{
    BlackPixelPolicy, Camera, DecodedPng, MOVING_SCALE, RenderError, apply_black_pixel_policy,
    decode_sized_png, terminal_source_row, upload_texture,
};
use crate::{
    actors::{Actor, Position},
    assets::{FIXED_GRAPHICS_PATH, MOVING_GRAPHICS_PATH},
    game::{Board, Game},
    platform::{
        rect::Rect,
        render::{BlendMode, Canvas, Texture, TextureCreator},
        video::{Window, WindowContext},
    },
};

/// Two reusable cell-state buffers, swapped only after a complete simulation frame.
struct CellFrames {
    /// Tick of the previously rendered state, including immediate commands between ticks.
    tick: u64,
    /// State that produced the previously rendered frame.
    previous: Board,
    /// Scratch storage populated with the next complete live board.
    current: Board,
}

/// CPU-side artwork and saved board pixels, independent of display refreshes.
struct LevelBitmap {
    /// Original 640×16 fixed atlas, decoded once per renderer.
    fixed: DecodedPng,
    /// Original 320×462 moving atlas, decoded without a color key.
    moving: DecodedPng,
    /// Seven complete Terminal tiles assembled once from the fixed atlas.
    terminal: DecodedPng,
    /// Previous/current typed cell storage; absent until a level is initialized.
    frames: Option<CellFrames>,
    /// Entire board at original 16-pixel tile resolution, including offscreen cells.
    pixels: DecodedPng,
    /// Whether resolved cell sprites require a texture upload at the next display.
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
        // Scrolling is an atlas selection at runtime, so every Terminal still
        // resolves to one sprite rather than a list of individual scanlines.
        let mut terminal = DecodedPng {
            width: 112,
            height: 16,
            pixels: vec![0; 112 * 16 * 4],
        };
        for phase in 0..7u8 {
            for y in 0..16u8 {
                for x in 0..16usize {
                    let row = if (2..=9).contains(&y) {
                        terminal_source_row(phase, y)
                    } else {
                        y
                    };
                    let source = (usize::from(row) * 640 + 19 * 16 + x) * 4;
                    let destination = (usize::from(y) * 112 + usize::from(phase) * 16 + x) * 4;
                    terminal.pixels[destination..destination + 4]
                        .copy_from_slice(&fixed.pixels[source..source + 4]);
                }
            }
        }
        Ok(Self {
            fixed,
            moving,
            terminal,
            frames: None,
            pixels: DecodedPng {
                width: 0,
                height: 0,
                pixels: Vec::new(),
            },
            dirty: false,
        })
    }

    /// Starts a fresh level image and initializes both cell buffers to the loaded board.
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
        // Seed terrain and loaded actor phases once. Later frames compare the
        // previous/current cells instead of reconstructing the whole bitmap.
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
            for blit in actor_sprites(state.actor()).iter() {
                self.copy(position, blit);
            }
        }
        self.frames = Some(CellFrames {
            tick: 0,
            previous: board.clone(),
            current: board.clone(),
        });
        self.dirty = true;
        Ok(())
    }

    /// Renders one previous/current cell pair and then swaps the reusable buffers.
    fn update(&mut self, game: &Game) {
        let mut frames = self
            .frames
            .take()
            .expect("reset must initialize cell buffers");
        if frames.tick == game.tick_count() && &frames.previous == game.board() {
            self.frames = Some(frames);
            return;
        }
        frames.current.copy_from(game.board());
        // Murphy's callback precedes the row-major actor pass. Completion uses
        // the old cell/action; movement can use the new destination cell.
        if let Some((position, sprites)) = murphy_sprites(&frames.previous, &frames.current) {
            for sprite in sprites.iter() {
                self.copy(position, sprite);
            }
        }
        for index in 0..frames.current.cells().len() {
            let position = frames.current.position(index).unwrap();
            // Immediate commands can replace cells without a simulation update.
            // Only their changed cells need resolution; unrelated actors must
            // keep the picture already displayed for this tick.
            if frames.tick == game.tick_count()
                && frames.previous.state(position) == frames.current.state(position)
            {
                continue;
            }
            for sprite in cell_sprites(
                &frames.previous,
                &frames.current,
                position,
                game.freeze_zonks(),
            )
            .iter()
            {
                self.copy(position, sprite);
            }
        }
        frames.tick = game.tick_count();
        std::mem::swap(&mut frames.previous, &mut frames.current);
        self.frames = Some(frames);
    }

    /// Copies one full opaque rectangle, clipped only at the level's outer edge.
    fn copy(&mut self, position: Position, blit: Blit) {
        let (atlas, part) = match blit {
            Blit::Fixed(part) => (&self.fixed, part),
            Blit::Moving(part) => (&self.moving, part),
            Blit::Terminal(part) => (&self.terminal, part),
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

/// Uploads the resolved level image independently of simulation frame cadence.
pub(super) struct LevelRenderer<'textures> {
    /// SDL context used when a new level needs a streaming texture.
    creator: &'textures TextureCreator<WindowContext>,
    /// Cell buffers and resolved CPU pixels, including offscreen actors.
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

    /// Resolves the current board before another simulation tick may advance it.
    pub(super) fn update(&mut self, game: &Game) {
        assert!(
            self.texture.is_some(),
            "begin_level must precede gameplay updates"
        );
        self.bitmap.update(game);
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
mod tests;
