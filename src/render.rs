//! SDL2 rendering for the row-major simulation and repacked sprite atlas.

use std::{error::Error, fmt, io::Cursor};

use sdl2::{
    pixels::{Color, PixelFormatEnum},
    rect::Rect,
    render::{BlendMode, Canvas, Texture, TextureCreator},
    video::{Window, WindowContext},
};

use crate::{
    actor::{
        Actor, AnimationKind, Direction, EnemyTurn, MurphyAnimation, MurphyMoveTarget, Position,
        State,
    },
    game::{Game, GameStatus},
    murphy_animation::{SourcePoint, SpritePart, sprite_parts},
};

/// Logical width used by the resizable SDL window.
pub const LOGICAL_WIDTH: u32 = 960;

/// Logical height used by the resizable SDL window.
pub const LOGICAL_HEIGHT: u32 = 640;

/// Height reserved for the two-line status panel.
const HUD_HEIGHT: u32 = 64;

/// Height of the scrolling board viewport above the HUD.
const VIEW_HEIGHT: u32 = LOGICAL_HEIGHT - HUD_HEIGHT;

/// Displayed width and height of one repacked sprite cell.
const TILE_SIZE: u32 = 32;

/// Source width and height of one `RocksSP.png` atlas cell.
const ATLAS_CELL_SIZE: u32 = 32;

/// Number of sprite cells across the supplied atlas.
const ATLAS_COLUMNS: u8 = 16;

/// Number of sprite cells down the supplied atlas.
const ATLAS_ROWS: u8 = 15;

/// Pixel width and height of one decoded font glyph.
const FONT_CELL_SIZE: u32 = 8;

/// Integer scale used to keep HUD lettering crisp and legible.
const FONT_SCALE: u32 = 2;

/// Number of glyphs placed horizontally in `CHARS8.DAT`.
const FONT_GLYPHS: u8 = 64;

/// Repacked, 2× Supaplex actor sprites supplied with the repository.
const ROCKS_SP_PNG: &[u8] = include_bytes!("../RocksSP.png");

/// Font converted from the original headerless `CHARS8.DAT` file.
const CHARS8_PNG: &[u8] = include_bytes!("../assets/chars8.png");

/// Pixel-perfect conversion of the original `MOVING.DAT` sprite sheet.
const MOVING_PNG: &[u8] = include_bytes!("../assets/moving.png");

/// Integer enlargement from original 16-pixel tiles to the 32-pixel board.
const MOVING_SCALE: u32 = 2;

/// Display-space distance Murphy advances during one original movement update.
const MURPHY_STEP: i32 = 2 * MOVING_SCALE as i32;

/// Grid location of one 32×32 source frame in `RocksSP.png`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SpriteCell {
    /// Zero-based atlas column.
    column: u8,
    /// Zero-based atlas row.
    row: u8,
}

impl SpriteCell {
    /// Creates an atlas cell whose bounds are checked by mapping tests.
    const fn new(column: u8, row: u8) -> Self {
        Self { column, row }
    }

    /// Converts grid coordinates to an SDL source rectangle.
    fn source(self) -> Rect {
        Rect::new(
            i32::from(self.column) * ATLAS_CELL_SIZE as i32,
            i32::from(self.row) * ATLAS_CELL_SIZE as i32,
            ATLAS_CELL_SIZE,
            ATLAS_CELL_SIZE,
        )
    }

    /// Reports whether the frame lies completely inside the known atlas grid.
    const fn is_valid(self) -> bool {
        self.column < ATLAS_COLUMNS && self.row < ATLAS_ROWS
    }
}

/// Pixel camera origin in full-board coordinates.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct Camera {
    /// Horizontal board pixel drawn at the viewport's left edge.
    x: i32,
    /// Vertical board pixel drawn at the viewport's top edge.
    y: i32,
}

/// Decoded RGBA8 image ready to upload to an SDL streaming texture.
#[derive(Clone, Debug, Eq, PartialEq)]
struct DecodedPng {
    /// Pixel width reported by the PNG frame header.
    width: u32,
    /// Pixel height reported by the PNG frame header.
    height: u32,
    /// Tightly packed RGBA bytes in scanline order.
    pixels: Vec<u8>,
}

/// Treatment applied to black source pixels before an image becomes an SDL texture.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BlackPixelPolicy {
    /// Preserve opaque black so a rectangular DOS blit can erase prior artwork.
    Opaque,
    /// Convert black to zero alpha for modern overlay-only atlas rendering.
    Transparent,
}

/// Textures and mapping logic needed to draw one game snapshot.
pub struct Renderer<'textures> {
    /// Repacked actor sprite atlas loaded from `RocksSP.png`.
    sprites: Texture<'textures>,
    /// Original variably sized movement frames used for Murphy composites.
    moving: Texture<'textures>,
    /// Original eight-pixel DOS font converted to an RGBA PNG.
    font: Texture<'textures>,
    /// Most recent camera centered on a live Murphy.
    ///
    /// A death transition replaces Murphy with an Explosion immediately. The
    /// retained camera keeps that blast visible instead of jumping to `(0, 0)`.
    camera: Camera,
}

impl<'textures> Renderer<'textures> {
    /// Decodes embedded PNG assets and uploads nearest-neighbor SDL textures.
    pub fn new(
        texture_creator: &'textures TextureCreator<WindowContext>,
    ) -> Result<Self, RenderError> {
        let sprites = load_texture(
            texture_creator,
            ROCKS_SP_PNG,
            512,
            480,
            "RocksSP.png",
            BlackPixelPolicy::Transparent,
        )?;
        let moving = load_texture(
            texture_creator,
            MOVING_PNG,
            320,
            462,
            "assets/moving.png",
            BlackPixelPolicy::Opaque,
        )?;
        let font = load_texture(
            texture_creator,
            CHARS8_PNG,
            512,
            8,
            "assets/chars8.png",
            BlackPixelPolicy::Transparent,
        )?;

        Ok(Self {
            sprites,
            moving,
            font,
            camera: Camera::default(),
        })
    }

    /// Draws the scrolling board, HUD, and completion/death overlay.
    pub fn draw(
        &mut self,
        canvas: &mut Canvas<Window>,
        game: &Game,
        level_number: usize,
    ) -> Result<(), RenderError> {
        // Every cell sprite uses black as its original DOS background. Clearing
        // first also supplies black behind pixels made transparent on upload.
        canvas.set_draw_color(Color::RGB(0, 0, 0));
        canvas.clear();

        // A live Murphy supplies a fresh target every frame. Terminal snapshots
        // have no Murphy, so they deliberately retain the last playable view.
        if let Some(camera) = camera_for(game) {
            self.camera = camera;
        }
        let camera = self.camera;

        // Logical destinations hold actors while their sprites interpolate
        // from a source cell. Paint stationary cells first and interpolated
        // actors second so row-major ordering cannot hide a leftward roll or
        // port traversal behind the terrain it visually overlaps.
        for moving_pass in [false, true] {
            for (index, state) in game.board().cells().iter().enumerate() {
                let is_interpolated = matches!(
                    state.animation().kind(),
                    AnimationKind::Moving(_)
                        | AnimationKind::Rolling(_)
                        | AnimationKind::OrangeFalling
                        | AnimationKind::SnikSnakMove(_)
                        | AnimationKind::ElectronMove(_)
                        | AnimationKind::Murphy(_)
                );
                if is_interpolated != moving_pass {
                    continue;
                }

                let position = game
                    .board()
                    .position(index)
                    .expect("enumerated board indices are always valid");
                self.draw_state(canvas, position, state, camera)?;
            }
        }

        self.draw_hud(canvas, game, level_number)?;
        self.draw_status_overlay(canvas, game.status())?;
        canvas.present();
        Ok(())
    }

    /// Draws one actor frame at its camera-relative, interpolated destination.
    fn draw_state(
        &mut self,
        canvas: &mut Canvas<Window>,
        position: Position,
        state: &State,
        camera: Camera,
    ) -> Result<(), RenderError> {
        // Empty and invisible cells intentionally leave the freshly cleared
        // black background untouched.
        if matches!(state.actor(), Actor::Empty(_) | Actor::InvisibleWall(_)) {
            return Ok(());
        }

        if matches!(
            state.animation().kind(),
            AnimationKind::Rolling(_)
                | AnimationKind::OrangeFalling
                | AnimationKind::Moving(Direction::Down)
        ) && matches!(
            state.actor(),
            Actor::Zonk(_) | Actor::Infotron(_) | Actor::OrangeDisk(_)
        ) {
            return self.draw_gravity_actor(canvas, position, state, camera);
        }

        if let AnimationKind::Murphy(action) = state.animation().kind() {
            return self.draw_murphy_animation(
                canvas,
                position,
                action,
                state.animation().frame(),
                camera,
            );
        }

        if matches!(state.actor(), Actor::SnikSnak(_))
            && matches!(
                state.animation().kind(),
                AnimationKind::SnikSnakTurn(_) | AnimationKind::SnikSnakMove(_)
            )
        {
            return self.draw_snik_snak_animation(canvas, position, state, camera);
        }
        if matches!(state.actor(), Actor::Electron(_))
            && matches!(
                state.animation().kind(),
                AnimationKind::ElectronTurn(_) | AnimationKind::ElectronMove(_)
            )
        {
            return self.draw_electron_animation(canvas, position, state, camera);
        }

        let sprite = sprite_for_state(state);
        debug_assert!(sprite.is_valid(), "sprite mapping must remain inside atlas");
        let (offset_x, offset_y) = movement_offset(state);
        let destination = Rect::new(
            position.x as i32 * TILE_SIZE as i32 - camera.x + offset_x,
            position.y as i32 * TILE_SIZE as i32 - camera.y + offset_y,
            TILE_SIZE,
            TILE_SIZE,
        );

        // Skip cells completely outside the board viewport, including movement
        // interpolation that temporarily crosses a viewport edge.
        let viewport = Rect::new(0, 0, LOGICAL_WIDTH, VIEW_HEIGHT);
        if !destination.has_intersection(viewport) {
            return Ok(());
        }

        canvas
            .copy(&self.sprites, sprite.source(), destination)
            .map_err(RenderError::Sdl)
    }

    /// Draws one original variably sized Murphy descriptor at two-times scale.
    fn draw_murphy_animation(
        &mut self,
        canvas: &mut Canvas<Window>,
        position: Position,
        action: MurphyAnimation,
        frame: u8,
        camera: Camera,
    ) -> Result<(), RenderError> {
        // The DOS level bitmap persisted between frames, so target pixels not
        // yet reached by a narrow vertical descriptor remained visible. This
        // stateless renderer must reconstruct that untouched target because its
        // logical cell already contains Murphy from frame zero. MOVING.DAT is
        // then copied opaquely over it: black pixels erase the traversed area,
        // and every final movement rectangle covers the complete target cell.
        if let MurphyAnimation::Move { target, .. } = action {
            let background_tile = match target {
                MurphyMoveTarget::Empty => None,
                MurphyMoveTarget::Base => Some(2),
                MurphyMoveTarget::Infotron => Some(4),
                MurphyMoveTarget::RedDisk | MurphyMoveTarget::PlantedRedDisk => Some(20),
            };
            if let Some(tile) = background_tile {
                // Draw only the semantic target cell. The following opaque
                // descriptor supplies the already-consumed portions and Murphy.
                let destination = Rect::new(
                    position.x as i32 * TILE_SIZE as i32 - camera.x,
                    position.y as i32 * TILE_SIZE as i32 - camera.y,
                    TILE_SIZE,
                    TILE_SIZE,
                );
                canvas
                    .copy(&self.sprites, static_sprite(tile).source(), destination)
                    .map_err(RenderError::Sdl)?;
            }
        }

        // Each descriptor is a complete opaque rectangle rather than a
        // transparent sprite layer, exactly matching the original byte copy.
        let parts = sprite_parts(action, frame);
        self.draw_murphy_part(canvas, position, parts.primary, camera)?;
        if let Some(secondary) = parts.secondary {
            self.draw_murphy_part(canvas, position, secondary, camera)?;
        }
        Ok(())
    }

    /// Copies one original-resolution Murphy layer into board pixel space.
    fn draw_murphy_part(
        &mut self,
        canvas: &mut Canvas<Window>,
        position: Position,
        part: SpritePart,
        camera: Camera,
    ) -> Result<(), RenderError> {
        let source = Rect::new(part.source.x, part.source.y, part.width, part.height);
        let destination = Rect::new(
            position.x as i32 * TILE_SIZE as i32 - camera.x + part.offset_x * MOVING_SCALE as i32,
            position.y as i32 * TILE_SIZE as i32 - camera.y + part.offset_y * MOVING_SCALE as i32,
            part.width * MOVING_SCALE,
            part.height * MOVING_SCALE,
        );

        // SDL clips wide push and vertical 18/34-pixel composites against the
        // viewport, preserving partial frames at camera edges without slicing
        // the descriptor tables themselves.
        canvas
            .copy(&self.moving, source, destination)
            .map_err(RenderError::Sdl)
    }

    /// Draws original Zonk, Infotron, or Orange fall/slide rectangles.
    fn draw_gravity_actor(
        &mut self,
        canvas: &mut Canvas<Window>,
        position: Position,
        state: &State,
        camera: Camera,
    ) -> Result<(), RenderError> {
        let Some(part) = gravity_sprite_part(
            state.actor(),
            state.animation().kind(),
            state.animation().frame(),
        ) else {
            debug_assert!(
                false,
                "gravity renderer received an unsupported actor phase"
            );
            return Ok(());
        };
        self.draw_murphy_part(canvas, position, part, camera)
    }

    /// Draws one exact original Snik Snak turn or transfer rectangle.
    fn draw_snik_snak_animation(
        &mut self,
        canvas: &mut Canvas<Window>,
        position: Position,
        state: &State,
        camera: Camera,
    ) -> Result<(), RenderError> {
        let Some(part) = snik_snak_sprite_part(state.animation().kind(), state.animation().frame())
        else {
            debug_assert!(false, "Snik Snak renderer received an unsupported phase");
            return Ok(());
        };
        // Enemy frames share the same colorkeyed MOVING.DAT conversion and
        // unscaled offset convention as Murphy and gravity actors.
        self.draw_murphy_part(canvas, position, part, camera)
    }

    /// Draws one exact original Electron turn or transfer rectangle.
    fn draw_electron_animation(
        &mut self,
        canvas: &mut Canvas<Window>,
        position: Position,
        state: &State,
        camera: Camera,
    ) -> Result<(), RenderError> {
        let Some(part) = electron_sprite_part(state.animation().kind(), state.animation().frame())
        else {
            debug_assert!(false, "Electron renderer received an unsupported phase");
            return Ok(());
        };
        // Electron artwork is copied from the same transparent MOVING.DAT
        // texture while retaining its own literal coordinate table.
        self.draw_murphy_part(canvas, position, part, camera)
    }

    /// Draws level metadata and live counters with the converted DOS font.
    fn draw_hud(
        &mut self,
        canvas: &mut Canvas<Window>,
        game: &Game,
        level_number: usize,
    ) -> Result<(), RenderError> {
        let hud_y = VIEW_HEIGHT as i32;
        canvas.set_draw_color(Color::RGB(34, 34, 42));
        canvas
            .fill_rect(Rect::new(0, hud_y, LOGICAL_WIDTH, HUD_HEIGHT))
            .map_err(RenderError::Sdl)?;
        canvas.set_draw_color(Color::RGB(122, 122, 132));
        canvas
            .fill_rect(Rect::new(0, hud_y, LOGICAL_WIDTH, 2))
            .map_err(RenderError::Sdl)?;

        let first_line = format!("LEVEL {level_number:03}  {}", game.title());
        let second_line = format!(
            "INFOTRONS {:03}  RED DISKS {:02}  GRAVITY {}  ZONKS {}",
            game.remaining_infotrons(),
            game.red_disks(),
            if game.gravity() { "ON" } else { "OFF" },
            if game.freeze_zonks() {
                "FROZEN"
            } else {
                "LIVE"
            },
        );
        self.draw_text(canvas, &first_line, 12, hud_y + 8, Color::RGB(255, 210, 40))?;
        self.draw_text(
            canvas,
            &second_line,
            12,
            hud_y + 34,
            Color::RGB(225, 225, 225),
        )
    }

    /// Draws a centered terminal-state banner while keeping the board visible.
    fn draw_status_overlay(
        &mut self,
        canvas: &mut Canvas<Window>,
        status: GameStatus,
    ) -> Result<(), RenderError> {
        let (message, color) = match status {
            GameStatus::Playing => return Ok(()),
            GameStatus::Completed => ("LEVEL COMPLETE - PRESS R", Color::RGB(80, 255, 120)),
            GameStatus::Dead => ("MURPHY DESTROYED - PRESS R", Color::RGB(255, 90, 70)),
        };

        let width = text_width(message) + 32;
        let x = (LOGICAL_WIDTH.saturating_sub(width) / 2) as i32;
        let y = (VIEW_HEIGHT / 2).saturating_sub(24) as i32;
        canvas.set_draw_color(Color::RGB(20, 20, 24));
        canvas
            .fill_rect(Rect::new(x, y, width, 48))
            .map_err(RenderError::Sdl)?;
        self.draw_text(canvas, message, x + 16, y + 16, color)
    }

    /// Draws supported ASCII text from the 64-glyph `CHARS8.DAT` strip.
    fn draw_text(
        &mut self,
        canvas: &mut Canvas<Window>,
        text: &str,
        x: i32,
        y: i32,
        color: Color,
    ) -> Result<(), RenderError> {
        self.font.set_color_mod(color.r, color.g, color.b);
        let glyph_size = (FONT_CELL_SIZE * FONT_SCALE) as i32;

        for (character_index, character) in text.chars().enumerate() {
            // The original strip covers ASCII space through underscore. Uppercase
            // folding keeps ordinary Rust strings representable by that range.
            let character = character.to_ascii_uppercase();
            let ascii = u32::from(character);
            let glyph = if (32..32 + u32::from(FONT_GLYPHS)).contains(&ascii) {
                ascii - 32
            } else {
                u32::from(b'?' - b' ')
            };
            let source = Rect::new(
                (glyph * FONT_CELL_SIZE) as i32,
                0,
                FONT_CELL_SIZE,
                FONT_CELL_SIZE,
            );
            let destination = Rect::new(
                x + character_index as i32 * glyph_size,
                y,
                FONT_CELL_SIZE * FONT_SCALE,
                FONT_CELL_SIZE * FONT_SCALE,
            );
            canvas
                .copy(&self.font, source, destination)
                .map_err(RenderError::Sdl)?;
        }

        Ok(())
    }
}

/// Selects one unscaled original source rectangle for a gravity-driven actor.
fn gravity_sprite_part(actor: &Actor, kind: AnimationKind, frame: u8) -> Option<SpritePart> {
    let frame = frame.min(7);
    let part = match (actor, kind) {
        (Actor::Zonk(_), AnimationKind::Moving(Direction::Down)) => SpritePart {
            source: crate::murphy_animation::SourcePoint { x: 224, y: 82 },
            width: 16,
            height: 18,
            offset_x: 0,
            offset_y: -16 + i32::from(frame) * 2,
        },
        (Actor::Infotron(_), AnimationKind::Moving(Direction::Down)) => SpritePart {
            source: crate::murphy_animation::SourcePoint { x: 240, y: 178 },
            width: 16,
            height: 18,
            offset_x: 0,
            offset_y: -16 + i32::from(frame) * 2,
        },
        (Actor::OrangeDisk(_), AnimationKind::OrangeFalling) => SpritePart {
            source: crate::murphy_animation::SourcePoint { x: 128, y: 64 },
            width: 16,
            height: 18,
            offset_x: 0,
            offset_y: i32::from(frame) * 2,
        },
        (Actor::Zonk(_), AnimationKind::Rolling(direction)) => {
            let source_y = if direction == Direction::Left {
                84
            } else {
                100
            };
            SpritePart {
                source: crate::murphy_animation::SourcePoint {
                    x: i32::from(frame) * 32,
                    y: source_y,
                },
                width: 32,
                height: 16,
                offset_x: if direction == Direction::Right {
                    -16
                } else {
                    0
                },
                offset_y: 0,
            }
        }
        (Actor::Infotron(_), AnimationKind::Rolling(direction)) => {
            // Frame four of the left strip really begins at x=8 in the
            // original pointer table. Preserve that historical coordinate.
            const LEFT_X: [i32; 8] = [0, 32, 64, 96, 8, 160, 192, 224];
            let (source_x, source_y) = if direction == Direction::Left {
                (LEFT_X[usize::from(frame)], 164)
            } else {
                (i32::from(frame) * 32, 180)
            };
            SpritePart {
                source: crate::murphy_animation::SourcePoint {
                    x: source_x,
                    y: source_y,
                },
                width: 32,
                height: 16,
                offset_x: if direction == Direction::Right {
                    -16
                } else {
                    0
                },
                offset_y: 0,
            }
        }
        _ => return None,
    };
    Some(part)
}

/// Original `MOVING.DAT` coordinates for all sixteen turn and thirty-two move frames.
///
/// The first two groups are counter-clockwise and clockwise turns. The final
/// four groups are transfers Up, Left, Down, and Right in original state order.
const SNIK_SNAK_SOURCE_POINTS: [SourcePoint; 48] = [
    SourcePoint { x: 192, y: 388 },
    SourcePoint { x: 64, y: 260 },
    SourcePoint { x: 96, y: 244 },
    SourcePoint { x: 80, y: 260 },
    SourcePoint { x: 208, y: 388 },
    SourcePoint { x: 96, y: 260 },
    SourcePoint { x: 48, y: 260 },
    SourcePoint { x: 112, y: 260 },
    SourcePoint { x: 192, y: 388 },
    SourcePoint { x: 112, y: 260 },
    SourcePoint { x: 48, y: 260 },
    SourcePoint { x: 96, y: 260 },
    SourcePoint { x: 208, y: 388 },
    SourcePoint { x: 80, y: 260 },
    SourcePoint { x: 96, y: 244 },
    SourcePoint { x: 64, y: 260 },
    SourcePoint { x: 0, y: 424 },
    SourcePoint { x: 16, y: 424 },
    SourcePoint { x: 32, y: 424 },
    SourcePoint { x: 48, y: 424 },
    SourcePoint { x: 64, y: 424 },
    SourcePoint { x: 80, y: 424 },
    SourcePoint { x: 96, y: 424 },
    SourcePoint { x: 112, y: 424 },
    SourcePoint { x: 192, y: 228 },
    SourcePoint { x: 224, y: 228 },
    SourcePoint { x: 256, y: 228 },
    SourcePoint { x: 288, y: 228 },
    SourcePoint { x: 0, y: 244 },
    SourcePoint { x: 32, y: 244 },
    SourcePoint { x: 64, y: 244 },
    SourcePoint { x: 96, y: 244 },
    SourcePoint { x: 144, y: 422 },
    SourcePoint { x: 160, y: 422 },
    SourcePoint { x: 176, y: 422 },
    SourcePoint { x: 192, y: 422 },
    SourcePoint { x: 208, y: 422 },
    SourcePoint { x: 224, y: 422 },
    SourcePoint { x: 240, y: 422 },
    SourcePoint { x: 256, y: 422 },
    SourcePoint { x: 128, y: 244 },
    SourcePoint { x: 160, y: 244 },
    SourcePoint { x: 192, y: 244 },
    SourcePoint { x: 224, y: 244 },
    SourcePoint { x: 256, y: 244 },
    SourcePoint { x: 288, y: 244 },
    SourcePoint { x: 0, y: 260 },
    SourcePoint { x: 32, y: 260 },
];

/// Selects one variably sized Snik Snak rectangle and its logical-cell offset.
fn snik_snak_sprite_part(kind: AnimationKind, frame: u8) -> Option<SpritePart> {
    let frame = frame.min(7);
    let (source_index, width, height, offset_x, offset_y) = match kind {
        AnimationKind::SnikSnakTurn(turn) => {
            let cycle_start = match turn {
                EnemyTurn::Left => 0,
                EnemyTurn::Right => 8,
            };
            (cycle_start + usize::from(frame), 16, 16, 0, 0)
        }
        AnimationKind::SnikSnakMove(Direction::Up) => {
            // Up states use gravity offsets one through eight, beginning two
            // original pixels above the retained source cell.
            (
                16 + usize::from(frame),
                16,
                18,
                0,
                14 - i32::from(frame) * 2,
            )
        }
        AnimationKind::SnikSnakMove(Direction::Left) => (24 + usize::from(frame), 32, 16, 0, 0),
        AnimationKind::SnikSnakMove(Direction::Down) => (
            32 + usize::from(frame),
            16,
            18,
            0,
            -16 + i32::from(frame) * 2,
        ),
        AnimationKind::SnikSnakMove(Direction::Right) => (40 + usize::from(frame), 32, 16, -16, 0),
        _ => return None,
    };
    Some(SpritePart {
        source: SNIK_SNAK_SOURCE_POINTS[source_index],
        width,
        height,
        offset_x,
        offset_y,
    })
}

/// Original `MOVING.DAT` coordinates for all Electron turn and transfer states.
///
/// Indices follow the state byte exactly: left turn, right turn, Up, Left,
/// Down, and Right. Some vertical source rows deliberately differ by one pixel.
const ELECTRON_SOURCE_POINTS: [SourcePoint; 48] = [
    SourcePoint { x: 0, y: 404 },
    SourcePoint { x: 16, y: 404 },
    SourcePoint { x: 32, y: 404 },
    SourcePoint { x: 48, y: 404 },
    SourcePoint { x: 64, y: 404 },
    SourcePoint { x: 80, y: 404 },
    SourcePoint { x: 96, y: 404 },
    SourcePoint { x: 112, y: 404 },
    SourcePoint { x: 0, y: 404 },
    SourcePoint { x: 112, y: 404 },
    SourcePoint { x: 96, y: 404 },
    SourcePoint { x: 80, y: 404 },
    SourcePoint { x: 64, y: 404 },
    SourcePoint { x: 48, y: 404 },
    SourcePoint { x: 32, y: 404 },
    SourcePoint { x: 16, y: 404 },
    SourcePoint { x: 144, y: 404 },
    SourcePoint { x: 160, y: 404 },
    SourcePoint { x: 176, y: 404 },
    SourcePoint { x: 192, y: 404 },
    SourcePoint { x: 208, y: 404 },
    SourcePoint { x: 224, y: 404 },
    SourcePoint { x: 240, y: 404 },
    SourcePoint { x: 256, y: 404 },
    SourcePoint { x: 0, y: 372 },
    SourcePoint { x: 32, y: 372 },
    SourcePoint { x: 64, y: 372 },
    SourcePoint { x: 96, y: 372 },
    SourcePoint { x: 128, y: 372 },
    SourcePoint { x: 160, y: 372 },
    SourcePoint { x: 192, y: 372 },
    SourcePoint { x: 224, y: 372 },
    SourcePoint { x: 0, y: 402 },
    SourcePoint { x: 16, y: 402 },
    SourcePoint { x: 32, y: 402 },
    SourcePoint { x: 48, y: 402 },
    SourcePoint { x: 64, y: 402 },
    SourcePoint { x: 80, y: 403 },
    SourcePoint { x: 96, y: 403 },
    SourcePoint { x: 112, y: 402 },
    SourcePoint { x: 256, y: 372 },
    SourcePoint { x: 288, y: 372 },
    SourcePoint { x: 0, y: 388 },
    SourcePoint { x: 32, y: 388 },
    SourcePoint { x: 64, y: 388 },
    SourcePoint { x: 96, y: 388 },
    SourcePoint { x: 128, y: 388 },
    SourcePoint { x: 160, y: 388 },
];

/// Selects one variably sized Electron rectangle and its logical-cell offset.
fn electron_sprite_part(kind: AnimationKind, frame: u8) -> Option<SpritePart> {
    let frame = frame.min(7);
    let (source_index, width, height, offset_x, offset_y) = match kind {
        AnimationKind::ElectronTurn(turn) => {
            let cycle_start = match turn {
                EnemyTurn::Left => 0,
                EnemyTurn::Right => 8,
            };
            (cycle_start + usize::from(frame), 16, 16, 0, 0)
        }
        AnimationKind::ElectronMove(Direction::Up) => (
            16 + usize::from(frame),
            16,
            18,
            0,
            14 - i32::from(frame) * 2,
        ),
        AnimationKind::ElectronMove(Direction::Left) => (24 + usize::from(frame), 32, 16, 0, 0),
        AnimationKind::ElectronMove(Direction::Down) => (
            32 + usize::from(frame),
            16,
            18,
            0,
            -16 + i32::from(frame) * 2,
        ),
        AnimationKind::ElectronMove(Direction::Right) => (40 + usize::from(frame), 32, 16, -16, 0),
        _ => return None,
    };
    Some(SpritePart {
        source: ELECTRON_SOURCE_POINTS[source_index],
        width,
        height,
        offset_x,
        offset_y,
    })
}

/// Returns the rendered width of one string in logical SDL pixels.
fn text_width(text: &str) -> u32 {
    // Saturating conversion and multiplication keep layout total even for an
    // unexpectedly huge title supplied by a custom level source.
    u32::try_from(text.chars().count())
        .unwrap_or(u32::MAX)
        .saturating_mul(FONT_CELL_SIZE * FONT_SCALE)
}

/// Loads one RGBA PNG, applies its requested black-pixel policy, and uploads it.
fn load_texture<'textures>(
    texture_creator: &'textures TextureCreator<WindowContext>,
    png_bytes: &[u8],
    expected_width: u32,
    expected_height: u32,
    asset_name: &'static str,
    black_pixel_policy: BlackPixelPolicy,
) -> Result<Texture<'textures>, RenderError> {
    // Validate the decoded geometry before applying any pixel transformation so
    // malformed embedded resources report their dimensions without mutation.
    let mut image = decode_png(png_bytes)?;
    if image.width != expected_width || image.height != expected_height {
        return Err(RenderError::UnexpectedDimensions {
            asset: asset_name,
            expected_width,
            expected_height,
            actual_width: image.width,
            actual_height: image.height,
        });
    }

    // MOVING.DAT requires opaque black because each frame was historically a
    // rectangular byte copy. Atlas and font images remain overlay-oriented and
    // use the modern colorkey behavior selected by their caller.
    apply_black_pixel_policy(&mut image.pixels, black_pixel_policy);

    // Upload the transformed straight-alpha bytes without filtering; the
    // logical-size canvas supplies the only integer enlargement afterwards.
    let mut texture = texture_creator
        .create_texture_streaming(PixelFormatEnum::RGBA32, image.width, image.height)
        .map_err(|error| RenderError::Sdl(error.to_string()))?;
    texture
        .update(None, &image.pixels, image.width as usize * 4)
        .map_err(|error| RenderError::Sdl(error.to_string()))?;
    texture.set_blend_mode(BlendMode::Blend);
    Ok(texture)
}

/// Applies opaque-copy or black-colorkey semantics to tightly packed RGBA pixels.
fn apply_black_pixel_policy(pixels: &mut [u8], policy: BlackPixelPolicy) {
    // Opaque images already carry the alpha bytes emitted by the asset
    // converter. Leaving them untouched preserves black as active erase data.
    if policy == BlackPixelPolicy::Opaque {
        return;
    }

    // Transparent images use pure black as their colorkey. Colored pixels keep
    // their original alpha so this transformation remains safe for a future
    // asset containing deliberately translucent non-black artwork.
    let (pixels, remainder) = pixels.as_chunks_mut::<4>();
    debug_assert!(
        remainder.is_empty(),
        "RGBA image must contain complete pixels"
    );
    for pixel in pixels {
        if pixel[0] == 0 && pixel[1] == 0 && pixel[2] == 0 {
            pixel[3] = 0;
        }
    }
}

/// Decodes one embedded PNG and requires a tightly packed RGBA8 output frame.
fn decode_png(bytes: &[u8]) -> Result<DecodedPng, RenderError> {
    let mut decoder = png::Decoder::new(Cursor::new(bytes));
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder.read_info().map_err(RenderError::Png)?;
    let buffer_size = reader
        .output_buffer_size()
        .ok_or(RenderError::ImageTooLarge)?;
    let mut pixels = vec![0; buffer_size];
    let output = reader.next_frame(&mut pixels).map_err(RenderError::Png)?;

    if output.color_type != png::ColorType::Rgba || output.bit_depth != png::BitDepth::Eight {
        return Err(RenderError::UnexpectedPngFormat {
            color_type: output.color_type,
            bit_depth: output.bit_depth,
        });
    }

    pixels.truncate(output.buffer_size());
    Ok(DecodedPng {
        width: output.width,
        height: output.height,
        pixels,
    })
}

/// Selects the best static or animated atlas frame for one complete cell state.
fn sprite_for_state(state: &State) -> SpriteCell {
    let frame = state.animation().frame();
    match state.animation().kind() {
        AnimationKind::Explosion => SpriteCell::new(8 + frame.min(7), 3),
        AnimationKind::ElectronExplosion => SpriteCell::new(8 + frame.min(7), 4),
        AnimationKind::Bug => bug_sprite(frame),
        AnimationKind::BugDormant => static_sprite(2),
        AnimationKind::Terminal => SpriteCell::new(frame.min(6), 10),
        AnimationKind::Murphy(action) => murphy_animation_sprite(action, frame),
        AnimationKind::Moving(direction) | AnimationKind::Rolling(direction) => {
            moving_sprite(state, direction, frame)
        }
        AnimationKind::Idle
        | AnimationKind::ZonkPreFall
        | AnimationKind::InfotronPreFall
        | AnimationKind::RoundedPreRoll(_)
        | AnimationKind::Vacating(_)
        | AnimationKind::MurphyPushTarget
        | AnimationKind::MurphyDestination
        | AnimationKind::RoundedSide
        | AnimationKind::RoundedDestination
        | AnimationKind::OrangePreFall
        | AnimationKind::OrangeFalling
        | AnimationKind::SnikSnakTurn(_)
        | AnimationKind::SnikSnakMove(_)
        | AnimationKind::SnikSnakVacating(_)
        | AnimationKind::ElectronTurn(_)
        | AnimationKind::ElectronMove(_)
        | AnimationKind::ElectronVacating(_)
        | AnimationKind::RedDiskFuse
        | AnimationKind::OrangeDiskFuse => static_sprite(state.actor().tile_code()),
    }
}

/// Selects presentation frames for an actor logically entering its destination.
fn moving_sprite(state: &State, direction: Direction, frame: u8) -> SpriteCell {
    match state.actor() {
        Actor::Zonk(_) if direction.is_horizontal() => zonk_moving_sprite(direction, frame),
        Actor::Infotron(_) if direction.is_horizontal() => infotron_moving_sprite(direction, frame),
        _ => static_sprite(state.actor().tile_code()),
    }
}

/// Selects the sprite drawn for one non-negative original Bug state.
///
/// Bug state zero is the ordinary fixed Bug tile.  On each eligible global
/// quarter tick, the original updater increments the state and indexes its
/// coordinate table with states one through thirteen.  States one through
/// eleven oscillate across four electrical frames, state twelve returns to the
/// fixed Bug tile, and state thirteen is visually indistinguishable from Base
/// even though it remains lethal until the next eligible update.
fn bug_sprite(frame: u8) -> SpriteCell {
    // The repacked atlas stores the four electrical pictures in row six,
    // columns eight through eleven.  Column twelve belongs to another actor;
    // selecting it was the visible Bug-to-Snik-Snak corruption reported by the
    // fidelity audit.
    match frame.min(13) {
        0 | 12 => static_sprite(25),
        1 | 11 => SpriteCell::new(8, 6),
        2 | 6 | 10 => SpriteCell::new(9, 6),
        3 | 5 | 7 | 9 => SpriteCell::new(10, 6),
        4 | 8 => SpriteCell::new(11, 6),
        13.. => static_sprite(2),
    }
}

/// Selects the atlas rotation order for a horizontally moving Zonk.
fn zonk_moving_sprite(direction: Direction, frame: u8) -> SpriteCell {
    // RocksSP stores four clockwise rotations from left to right. Its original
    // metadata reverses that strip for left motion and begins right motion at
    // frame one, wrapping after the final cell.
    let phase = frame % 4;
    let strip_frame = match direction {
        Direction::Left => 3 - phase,
        Direction::Right => (phase + 1) % 4,
        Direction::Up | Direction::Down => return static_sprite(1),
    };
    SpriteCell::new(strip_frame, 6)
}

/// Selects four evenly sampled rotation frames for a moving Infotron.
fn infotron_moving_sprite(direction: Direction, frame: u8) -> SpriteCell {
    // The source strip contains eight frames while movement has four simulation
    // phases. Left motion samples even frames forward; right motion follows the
    // atlas's reverse-from-six metadata to produce the mirrored rotation.
    let forward_frame = frame.min(3) * 2;
    let strip_frame = match direction {
        Direction::Left => forward_frame,
        Direction::Right => 6 - forward_frame,
        Direction::Up | Direction::Down => return static_sprite(4),
    };
    SpriteCell::new(8 + strip_frame, 13)
}

/// Supplies an atlas fallback for one semantic original Murphy descriptor.
fn murphy_animation_sprite(action: MurphyAnimation, frame: u8) -> SpriteCell {
    let direction = match action {
        MurphyAnimation::Move { direction, .. }
        | MurphyAnimation::Snap { direction, .. }
        | MurphyAnimation::Push { direction, .. }
        | MurphyAnimation::Port { direction } => direction,
        MurphyAnimation::Exit | MurphyAnimation::PlantRedDisk => return static_sprite(3),
    };

    if matches!(action, MurphyAnimation::Move { .. }) {
        return match direction {
            Direction::Left => SpriteCell::new(8 + ping_pong(frame, 3), 0),
            Direction::Right => SpriteCell::new(11 + ping_pong(frame, 3), 0),
            Direction::Up | Direction::Down => static_sprite(3),
        };
    }

    match direction {
        Direction::Right => SpriteCell::new(8, 1),
        Direction::Left => SpriteCell::new(9, 1),
        Direction::Up => SpriteCell::new(14, 0),
        Direction::Down => SpriteCell::new(15, 0),
    }
}

/// Maps a serialized tile code to its static `RocksSP.png` grid cell.
fn static_sprite(tile: u8) -> SpriteCell {
    match tile {
        0 => SpriteCell::new(0, 0),
        1..=7 => SpriteCell::new(tile, 0),
        8 => SpriteCell::new(0, 1),
        9..=12 => SpriteCell::new(tile - 8, 1),
        13..=16 => SpriteCell::new(tile - 12, 1),
        17 => SpriteCell::new(1, 2),
        18 => SpriteCell::new(2, 2),
        19 => SpriteCell::new(0, 10),
        20 => SpriteCell::new(4, 2),
        21..=23 => SpriteCell::new(tile - 16, 2),
        24 => SpriteCell::new(8, 10),
        25 => SpriteCell::new(1, 3),
        26..=27 => SpriteCell::new(tile - 24, 3),
        28..=31 => SpriteCell::new(tile - 24, 3),
        32 => SpriteCell::new(0, 4),
        33..=37 => SpriteCell::new(tile - 32, 4),
        38..=39 => SpriteCell::new(tile - 32, 4),
        _ => SpriteCell::new(0, 0),
    }
}

/// Converts an animation frame to a forward-then-back strip index.
fn ping_pong(frame: u8, frame_count: u8) -> u8 {
    // A one-frame strip is stable. Larger strips have a period that does not
    // duplicate their two endpoints when reversing direction.
    if frame_count <= 1 {
        return 0;
    }
    let period = frame_count * 2 - 2;
    let phase = frame % period;
    if phase < frame_count {
        phase
    } else {
        period - phase
    }
}

/// Returns the sub-cell pixel offset implied by a movement animation.
fn movement_offset(state: &State) -> (i32, i32) {
    // Ordinary moving actors interpolate between inclusive endpoints because
    // their frame zero represents the untouched source position. Murphy is
    // handled separately below: his original routine advances its persistent
    // pixel position before drawing frame zero, so reusing this progress value
    // would insert a motionless update at the start of every Murphy action.
    let remaining = 1.0 - state.animation().progress();
    let distance = (remaining * TILE_SIZE as f32).round() as i32;
    match state.animation().kind() {
        AnimationKind::Moving(direction) => match direction {
            Direction::Up => (0, distance),
            Direction::Right => (-distance, 0),
            Direction::Down => (0, -distance),
            Direction::Left => (distance, 0),
        },
        AnimationKind::Rolling(direction) => match direction {
            Direction::Left => (distance, -distance),
            Direction::Right => (-distance, -distance),
            Direction::Up | Direction::Down => (0, 0),
        },
        AnimationKind::Murphy(action) => murphy_movement_offset(action, state.animation().frame()),
        _ => (0, 0),
    }
}

/// Reconstructs Murphy's original persistent pixel position for one action frame.
fn murphy_movement_offset(action: MurphyAnimation, frame: u8) -> (i32, i32) {
    // The DOS routine adds `speedX` and `speedY` before it draws the selected
    // coordinate. Consequently frame zero has already travelled two original
    // pixels, or four pixels after this renderer's integer enlargement. Limit
    // ordinary travel to one tile so the historical ninth right-Red-Disk image
    // remains duplicated artwork instead of moving the camera beyond Murphy's
    // logical destination.
    let updates = i32::from(frame) + 1;
    let travelled = (updates * MURPHY_STEP).min(TILE_SIZE as i32);

    match action {
        MurphyAnimation::Move { direction, .. } => {
            // A moving Murphy is stored at his destination from frame zero.
            // Subtract the distance not yet travelled to recover the original
            // pixel position between the reserved source and destination.
            let remaining = TILE_SIZE as i32 - travelled;
            match direction {
                Direction::Up => (0, remaining),
                Direction::Right => (-remaining, 0),
                Direction::Down => (0, -remaining),
                Direction::Left => (remaining, 0),
            }
        }
        MurphyAnimation::Push { direction, .. } => {
            // Push animations remain anchored at Murphy's source cell until
            // their final collision update transfers him into the target cell.
            match direction {
                Direction::Up => (0, -travelled),
                Direction::Right => (travelled, 0),
                Direction::Down => (0, travelled),
                Direction::Left => (-travelled, 0),
            }
        }
        MurphyAnimation::Port { direction } => {
            // Port descriptors cross two cells in the same eight updates, so
            // their original velocity is twice ordinary Murphy movement.
            let port_travelled = travelled * 2;
            match direction {
                Direction::Up => (0, -port_travelled),
                Direction::Right => (port_travelled, 0),
                Direction::Down => (0, port_travelled),
                Direction::Left => (-port_travelled, 0),
            }
        }
        // Snapping, planting, and exiting select changing artwork without
        // changing Murphy's persistent world-space position.
        MurphyAnimation::Snap { .. } | MurphyAnimation::Exit | MurphyAnimation::PlantRedDisk => {
            (0, 0)
        }
    }
}

/// Centers and clamps a camera around Murphy's interpolated visual position.
fn camera_for(game: &Game) -> Option<Camera> {
    let board_width = game.board().width() as i32 * TILE_SIZE as i32;
    let board_height = game.board().height() as i32 * TILE_SIZE as i32;
    let maximum_x = (board_width - LOGICAL_WIDTH as i32).max(0);
    let maximum_y = (board_height - VIEW_HEIGHT as i32).max(0);

    // Terminal snapshots intentionally return `None`, allowing `Renderer` to
    // preserve the last camera from the instant before Murphy disappeared.
    let position = game.murphy_position()?;
    let state = game
        .board()
        .state(position)
        .expect("reported Murphy position must contain a state");
    let (offset_x, offset_y) = movement_offset(state);
    let center_x = position.x as i32 * TILE_SIZE as i32 + offset_x + TILE_SIZE as i32 / 2;
    let center_y = position.y as i32 * TILE_SIZE as i32 + offset_y + TILE_SIZE as i32 / 2;

    Some(Camera {
        x: (center_x - LOGICAL_WIDTH as i32 / 2).clamp(0, maximum_x),
        y: (center_y - VIEW_HEIGHT as i32 / 2).clamp(0, maximum_y),
    })
}

/// Errors produced while decoding assets, uploading textures, or drawing SDL.
#[derive(Debug)]
pub enum RenderError {
    /// The PNG decoder rejected an embedded asset.
    Png(png::DecodingError),
    /// Decoded dimensions did not match the asset contract.
    UnexpectedDimensions {
        /// Human-readable embedded asset name.
        asset: &'static str,
        /// Required pixel width.
        expected_width: u32,
        /// Required pixel height.
        expected_height: u32,
        /// Decoded pixel width.
        actual_width: u32,
        /// Decoded pixel height.
        actual_height: u32,
    },
    /// The decoder could not represent the output buffer size on this platform.
    ImageTooLarge,
    /// A PNG decoded successfully but not into the required RGBA8 representation.
    UnexpectedPngFormat {
        /// Decoder-selected color type.
        color_type: png::ColorType,
        /// Decoder-selected component depth.
        bit_depth: png::BitDepth,
    },
    /// SDL rejected texture creation, upload, or a draw operation.
    Sdl(String),
}

impl fmt::Display for RenderError {
    /// Formats an actionable asset or SDL failure.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Png(error) => write!(formatter, "could not decode embedded PNG: {error}"),
            Self::UnexpectedDimensions {
                asset,
                expected_width,
                expected_height,
                actual_width,
                actual_height,
            } => write!(
                formatter,
                "{asset} is {actual_width}x{actual_height}; expected {expected_width}x{expected_height}"
            ),
            Self::ImageTooLarge => {
                formatter.write_str("decoded PNG is too large for this platform")
            }
            Self::UnexpectedPngFormat {
                color_type,
                bit_depth,
            } => write!(
                formatter,
                "embedded PNG decoded as {color_type:?}/{bit_depth:?}; expected RGBA/8-bit"
            ),
            Self::Sdl(error) => write!(formatter, "SDL rendering failed: {error}"),
        }
    }
}

impl Error for RenderError {
    /// Exposes the PNG decoder source while string-based SDL errors have none.
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Png(error) => Some(error),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    //! Pure mapping and decoding tests that do not initialize SDL video.

    use super::{
        ATLAS_COLUMNS, ATLAS_ROWS, BlackPixelPolicy, CHARS8_PNG, MOVING_PNG, ROCKS_SP_PNG,
        apply_black_pixel_policy, bug_sprite, decode_png, electron_sprite_part,
        gravity_sprite_part, infotron_moving_sprite, murphy_movement_offset, ping_pong,
        snik_snak_sprite_part, static_sprite, zonk_moving_sprite,
    };
    use crate::actor::{
        Actor, AnimationKind, Direction, EnemyTurn, Infotron, MurphyAnimation, MurphyMoveTarget,
        MurphyPushTarget, OrangeDisk, Zonk,
    };

    /// Confirms all embedded resources decode to their contracted RGBA sizes.
    #[test]
    fn embedded_render_assets_are_valid_rgba_pngs() {
        let sprites = decode_png(ROCKS_SP_PNG).expect("sprite atlas should decode");
        let moving = decode_png(MOVING_PNG).expect("original moving sheet should decode");
        let font = decode_png(CHARS8_PNG).expect("font should decode");

        assert_eq!((sprites.width, sprites.height), (512, 480));
        assert_eq!((moving.width, moving.height), (320, 462));
        assert_eq!((font.width, font.height), (512, 8));
    }

    /// Confirms opaque DOS copies and transparent overlays treat black differently.
    #[test]
    fn black_pixel_policy_preserves_moving_erase_pixels() {
        let source = [0, 0, 0, 255, 12, 34, 56, 192];
        let mut opaque = source;
        let mut transparent = source;

        // Opaque moving frames use black to erase the previously drawn target,
        // while overlay atlases discard only pure black and retain colored alpha.
        apply_black_pixel_policy(&mut opaque, BlackPixelPolicy::Opaque);
        apply_black_pixel_policy(&mut transparent, BlackPixelPolicy::Transparent);

        assert_eq!(opaque, source);
        assert_eq!(transparent, [0, 0, 0, 0, 12, 34, 56, 192]);
    }

    /// Confirms every completed Base-eating rectangle carries opaque black background pixels.
    #[test]
    fn final_base_movement_frames_contain_opaque_erase_data() {
        let mut moving = decode_png(MOVING_PNG).expect("MOVING.DAT conversion should decode");
        apply_black_pixel_policy(&mut moving.pixels, BlackPixelPolicy::Opaque);
        let variants = [
            (Direction::Up, true),
            (Direction::Up, false),
            (Direction::Right, false),
            (Direction::Down, true),
            (Direction::Down, false),
            (Direction::Left, true),
        ];

        for (direction, looking_left) in variants {
            let action = MurphyAnimation::Move {
                direction,
                target: MurphyMoveTarget::Base,
                looking_left,
            };
            let part = crate::murphy_animation::sprite_parts(action, 7).primary;

            // The renderer may reconstruct Base pixels outside the descriptor
            // during earlier frames, but the final opaque rectangle must span
            // the entire logical target so none can survive underneath it.
            assert!(part.offset_x <= 0);
            assert!(part.offset_y <= 0);
            assert!(part.offset_x + part.width as i32 >= 16);
            assert!(part.offset_y + part.height as i32 >= 16);

            // Search only the final descriptor rectangle. At least one opaque
            // black pixel must survive so copying this frame can cover the
            // consumed Base rather than reveal a synthetic tile underneath.
            let mut opaque_black_pixels = 0;
            for y in part.source.y..part.source.y + part.height as i32 {
                for x in part.source.x..part.source.x + part.width as i32 {
                    let pixel_index = (y as usize * moving.width as usize + x as usize) * 4;
                    let pixel = &moving.pixels[pixel_index..pixel_index + 4];
                    if pixel == [0, 0, 0, 255] {
                        opaque_black_pixels += 1;
                    }
                }
            }

            assert!(
                opaque_black_pixels > 0,
                "final {direction:?} Base frame must retain black erase pixels"
            );
        }
    }

    /// Confirms every serialized tile maps inside the 16×15 atlas grid.
    #[test]
    fn every_static_tile_mapping_stays_inside_atlas() {
        for tile in 0..=40 {
            let sprite = static_sprite(tile);
            assert!(
                sprite.column < ATLAS_COLUMNS,
                "tile {tile} column escaped atlas"
            );
            assert!(sprite.row < ATLAS_ROWS, "tile {tile} row escaped atlas");
        }
    }

    /// Confirms cyclic frame selection reverses without duplicating endpoints.
    #[test]
    fn ping_pong_frames_return_to_the_start() {
        let frames = (0..8).map(|frame| ping_pong(frame, 4)).collect::<Vec<_>>();

        assert_eq!(frames, vec![0, 1, 2, 3, 2, 1, 0, 1]);
    }

    /// Confirms chained movement advances on frame zero instead of pausing at a tile boundary.
    #[test]
    fn murphy_camera_motion_has_no_duplicate_boundary_sample() {
        let movement = MurphyAnimation::Move {
            direction: Direction::Right,
            target: MurphyMoveTarget::Empty,
            looking_left: false,
        };
        let tile = super::TILE_SIZE as i32;

        // Each state is anchored at its logical destination. The completed
        // first move reaches that anchor, while frame zero of the following
        // move is already four display pixels beyond the shared boundary.
        let completed_world_x = 5 * tile + murphy_movement_offset(movement, 7).0;
        let next_started_world_x = 6 * tile + murphy_movement_offset(movement, 0).0;

        assert_eq!(murphy_movement_offset(movement, 0), (-28, 0));
        assert_eq!(murphy_movement_offset(movement, 7), (0, 0));
        assert_eq!(next_started_world_x - completed_world_x, 4);
    }

    /// Confirms every cardinal move uses the original two-pixel unscaled velocity.
    #[test]
    fn murphy_move_camera_offsets_advance_four_display_pixels_per_update() {
        let cases = [
            (Direction::Up, (0, 28), (0, 0)),
            (Direction::Right, (-28, 0), (0, 0)),
            (Direction::Down, (0, -28), (0, 0)),
            (Direction::Left, (28, 0), (0, 0)),
        ];

        for (direction, first, last) in cases {
            let movement = MurphyAnimation::Move {
                direction,
                target: MurphyMoveTarget::Empty,
                looking_left: false,
            };

            assert_eq!(murphy_movement_offset(movement, 0), first);
            assert_eq!(murphy_movement_offset(movement, 7), last);
        }
    }

    /// Confirms source-anchored pushes and ports retain their distinct original speeds.
    #[test]
    fn murphy_push_and_port_camera_offsets_start_on_their_first_frames() {
        let push = MurphyAnimation::Push {
            direction: Direction::Left,
            target: MurphyPushTarget::Zonk,
        };
        let port = MurphyAnimation::Port {
            direction: Direction::Down,
        };

        assert_eq!(murphy_movement_offset(push, 0), (-4, 0));
        assert_eq!(murphy_movement_offset(push, 7), (-32, 0));
        assert_eq!(murphy_movement_offset(port, 0), (0, 8));
        assert_eq!(murphy_movement_offset(port, 7), (0, 64));
    }

    /// Confirms the ninth rightward Red Disk picture does not overshoot its destination.
    #[test]
    fn murphy_red_disk_ninth_picture_keeps_the_camera_on_the_tile() {
        let movement = MurphyAnimation::Move {
            direction: Direction::Right,
            target: MurphyMoveTarget::RedDisk,
            looking_left: false,
        };

        assert_eq!(murphy_movement_offset(movement, 7), (0, 0));
        assert_eq!(murphy_movement_offset(movement, 8), (0, 0));
    }

    /// Confirms every non-negative Bug state follows the original coordinate table.
    #[test]
    fn bug_frames_follow_the_original_spark_and_base_sequence() {
        let active = (0..14).map(bug_sprite).collect::<Vec<_>>();

        assert_eq!(
            active,
            vec![
                static_sprite(25),
                super::SpriteCell::new(8, 6),
                super::SpriteCell::new(9, 6),
                super::SpriteCell::new(10, 6),
                super::SpriteCell::new(11, 6),
                super::SpriteCell::new(10, 6),
                super::SpriteCell::new(9, 6),
                super::SpriteCell::new(10, 6),
                super::SpriteCell::new(11, 6),
                super::SpriteCell::new(10, 6),
                super::SpriteCell::new(9, 6),
                super::SpriteCell::new(8, 6),
                static_sprite(25),
                static_sprite(2),
            ]
        );
    }

    /// Confirms horizontal rolling follows the direction metadata of each strip.
    #[test]
    fn rolling_frames_reverse_between_left_and_right_motion() {
        let zonk_left = (0..4)
            .map(|frame| zonk_moving_sprite(Direction::Left, frame).column)
            .collect::<Vec<_>>();
        let zonk_right = (0..4)
            .map(|frame| zonk_moving_sprite(Direction::Right, frame).column)
            .collect::<Vec<_>>();
        let infotron_left = (0..4)
            .map(|frame| infotron_moving_sprite(Direction::Left, frame).column)
            .collect::<Vec<_>>();
        let infotron_right = (0..4)
            .map(|frame| infotron_moving_sprite(Direction::Right, frame).column)
            .collect::<Vec<_>>();

        assert_eq!(zonk_left, vec![3, 2, 1, 0]);
        assert_eq!(zonk_right, vec![1, 2, 3, 0]);
        assert_eq!(infotron_left, vec![8, 10, 12, 14]);
        assert_eq!(infotron_right, vec![14, 12, 10, 8]);
    }

    /// Confirms falling actors use the original two-pixel gravity increments.
    #[test]
    fn gravity_frames_stop_two_pixels_before_the_destination_tile() {
        let zonk = Actor::Zonk(Zonk::resting());
        let first = gravity_sprite_part(&zonk, AnimationKind::Moving(Direction::Down), 0)
            .expect("Zonk fall frame should map");
        let last = gravity_sprite_part(&zonk, AnimationKind::Moving(Direction::Down), 7)
            .expect("Zonk fall frame should map");

        assert_eq!(first.offset_y, -16);
        assert_eq!(last.offset_y, -2);
        assert_eq!((first.source.x, first.source.y), (224, 82));
    }

    /// Confirms each gravity actor selects its own unscaled source picture.
    #[test]
    fn falling_actor_sources_remain_distinct() {
        let infotron = gravity_sprite_part(
            &Actor::Infotron(Infotron::resting()),
            AnimationKind::Moving(Direction::Down),
            0,
        )
        .expect("Infotron fall frame should map");
        let orange = gravity_sprite_part(
            &Actor::OrangeDisk(OrangeDisk::resting()),
            AnimationKind::OrangeFalling,
            0,
        )
        .expect("Orange fall frame should map");

        assert_eq!((infotron.source.x, infotron.source.y), (240, 178));
        assert_eq!((orange.source.x, orange.source.y), (128, 64));
        assert_eq!(orange.offset_y, 0);
    }

    /// Confirms Snik Snak turns and moves use the literal MOVING.DAT rectangles.
    #[test]
    fn snik_snak_frames_preserve_turn_order_and_wide_horizontal_composites() {
        let turn = snik_snak_sprite_part(AnimationKind::SnikSnakTurn(EnemyTurn::Left), 2)
            .expect("left-turn frame should map");
        let move_left = snik_snak_sprite_part(AnimationKind::SnikSnakMove(Direction::Left), 7)
            .expect("left movement frame should map");
        let move_up = snik_snak_sprite_part(AnimationKind::SnikSnakMove(Direction::Up), 0)
            .expect("up movement frame should map");

        assert_eq!((turn.source.x, turn.source.y), (96, 244));
        assert_eq!((move_left.source.x, move_left.source.y), (96, 244));
        assert_eq!((move_left.width, move_left.height), (32, 16));
        assert_eq!(
            (move_up.width, move_up.height, move_up.offset_y),
            (16, 18, 14)
        );
    }

    /// Confirms Electron frames retain exact coordinates and vertical row quirks.
    #[test]
    fn electron_frames_preserve_reverse_turns_and_literal_vertical_sources() {
        let right_turn = electron_sprite_part(AnimationKind::ElectronTurn(EnemyTurn::Right), 1)
            .expect("right-turn frame should map");
        let down_five = electron_sprite_part(AnimationKind::ElectronMove(Direction::Down), 5)
            .expect("down movement frame should map");
        let move_right = electron_sprite_part(AnimationKind::ElectronMove(Direction::Right), 1)
            .expect("right movement frame should map");

        assert_eq!((right_turn.source.x, right_turn.source.y), (112, 404));
        assert_eq!((down_five.source.x, down_five.source.y), (80, 403));
        assert_eq!((down_five.width, down_five.height), (16, 18));
        assert_eq!((move_right.width, move_right.offset_x), (32, -16));
    }
}
