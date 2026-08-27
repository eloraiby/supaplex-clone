//! SDL2 rendering for the row-major simulation and repacked sprite atlas.

use std::{error::Error, fmt, io::Cursor};

use sdl2::{
    pixels::{Color, PixelFormatEnum},
    rect::Rect,
    render::{BlendMode, Canvas, Texture, TextureCreator},
    video::{Window, WindowContext},
};

use crate::{
    actor::{Actor, AnimationKind, Direction, Position, State},
    game::{Game, GameStatus},
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

/// Textures and mapping logic needed to draw one game snapshot.
pub struct Renderer<'textures> {
    /// Repacked actor sprite atlas loaded from `RocksSP.png`.
    sprites: Texture<'textures>,
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
        let sprites = load_texture(texture_creator, ROCKS_SP_PNG, 512, 480, "RocksSP.png")?;
        let font = load_texture(texture_creator, CHARS8_PNG, 512, 8, "assets/chars8.png")?;

        Ok(Self {
            sprites,
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
                        | AnimationKind::PortTraversal(_)
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

/// Returns the rendered width of one string in logical SDL pixels.
fn text_width(text: &str) -> u32 {
    // Saturating conversion and multiplication keep layout total even for an
    // unexpectedly huge title supplied by a custom level source.
    u32::try_from(text.chars().count())
        .unwrap_or(u32::MAX)
        .saturating_mul(FONT_CELL_SIZE * FONT_SCALE)
}

/// Loads one RGBA PNG, applies the DOS black colorkey, and uploads a texture.
fn load_texture<'textures>(
    texture_creator: &'textures TextureCreator<WindowContext>,
    png_bytes: &[u8],
    expected_width: u32,
    expected_height: u32,
    asset_name: &'static str,
) -> Result<Texture<'textures>, RenderError> {
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

    // The supplied artwork is fully opaque even though black is its intended
    // transparent colorkey. Removing black permits moving sprites to interpolate
    // over the cleared board without drawing a travelling black rectangle.
    let (pixels, remainder) = image.pixels.as_chunks_mut::<4>();
    debug_assert!(
        remainder.is_empty(),
        "RGBA image must contain complete pixels"
    );
    for pixel in pixels {
        if pixel[0] == 0 && pixel[1] == 0 && pixel[2] == 0 {
            pixel[3] = 0;
        }
    }

    let mut texture = texture_creator
        .create_texture_streaming(PixelFormatEnum::RGBA32, image.width, image.height)
        .map_err(|error| RenderError::Sdl(error.to_string()))?;
    texture
        .update(None, &image.pixels, image.width as usize * 4)
        .map_err(|error| RenderError::Sdl(error.to_string()))?;
    texture.set_blend_mode(BlendMode::Blend);
    Ok(texture)
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
        AnimationKind::Electron => SpriteCell::new(8 + frame.min(7), 10),
        AnimationKind::Terminal => SpriteCell::new(frame.min(6), 10),
        AnimationKind::SnikSnak => snik_sprite(state, frame),
        AnimationKind::Snapping(direction) => murphy_action_sprite(direction),
        AnimationKind::Moving(direction)
        | AnimationKind::Rolling(direction)
        | AnimationKind::PortTraversal(direction) => moving_sprite(state, direction, frame),
        AnimationKind::Idle
        | AnimationKind::ZonkPreFall
        | AnimationKind::Vacating(_)
        | AnimationKind::RedDiskFuse
        | AnimationKind::OrangeDiskFuse => static_sprite(state.actor().tile_code()),
    }
}

/// Selects presentation frames for an actor logically entering its destination.
fn moving_sprite(state: &State, direction: Direction, frame: u8) -> SpriteCell {
    match state.actor() {
        Actor::Murphy(_) => match direction {
            Direction::Left => SpriteCell::new(8 + ping_pong(frame, 3), 0),
            Direction::Right => SpriteCell::new(11 + ping_pong(frame, 3), 0),
            Direction::Up | Direction::Down => static_sprite(3),
        },
        Actor::Zonk(_) if direction.is_horizontal() => zonk_moving_sprite(direction, frame),
        Actor::Infotron(_) if direction.is_horizontal() => infotron_moving_sprite(direction, frame),
        Actor::SnikSnak(_) => snik_direction_sprite(direction, frame),
        Actor::Electron(_) => SpriteCell::new(8 + frame.saturating_mul(2).min(7), 10),
        _ => static_sprite(state.actor().tile_code()),
    }
}

/// Selects the original fourteen-frame active Bug presentation sequence.
fn bug_sprite(frame: u8) -> SpriteCell {
    // Frame thirteen deliberately looks exactly like safe Base even though its
    // `AnimationKind::Bug` remains lethal until the following Bug update. This
    // visual ambiguity is part of the original timing rather than a collision
    // shortcut based on sprite identity.
    match frame.min(13) {
        0 | 12 => SpriteCell::new(8, 6),
        1 | 11 => SpriteCell::new(9, 6),
        2 | 6 | 10 => SpriteCell::new(10, 6),
        3 | 5 | 7 | 9 => SpriteCell::new(11, 6),
        4 | 8 => SpriteCell::new(12, 6),
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

/// Selects a Snik Snak strip from its persistent heading.
fn snik_sprite(state: &State, frame: u8) -> SpriteCell {
    let direction = match state.actor() {
        Actor::SnikSnak(actor) => actor.heading(),
        _ => Direction::Left,
    };
    snik_direction_sprite(direction, frame)
}

/// Selects the four-frame strip corresponding to one Snik Snak direction.
fn snik_direction_sprite(direction: Direction, frame: u8) -> SpriteCell {
    let (column, row) = match direction {
        Direction::Left => (8, 8),
        Direction::Right => (12, 8),
        Direction::Up => (8, 9),
        Direction::Down => (12, 9),
    };
    SpriteCell::new(column + frame.min(3), row)
}

/// Selects one of the four single-frame Murphy snapping poses.
fn murphy_action_sprite(direction: Direction) -> SpriteCell {
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
    // The actor already occupies its logical destination. At frame zero it is
    // drawn at its prior logical location, then approaches zero displacement.
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
        AnimationKind::PortTraversal(direction) => match direction {
            Direction::Up => (0, distance * 2),
            Direction::Right => (-distance * 2, 0),
            Direction::Down => (0, -distance * 2),
            Direction::Left => (distance * 2, 0),
        },
        _ => (0, 0),
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
        ATLAS_COLUMNS, ATLAS_ROWS, CHARS8_PNG, ROCKS_SP_PNG, bug_sprite, decode_png,
        infotron_moving_sprite, ping_pong, static_sprite, zonk_moving_sprite,
    };
    use crate::actor::Direction;

    /// Confirms both embedded resources decode to their contracted RGBA sizes.
    #[test]
    fn embedded_render_assets_are_valid_rgba_pngs() {
        let sprites = decode_png(ROCKS_SP_PNG).expect("sprite atlas should decode");
        let font = decode_png(CHARS8_PNG).expect("font should decode");

        assert_eq!((sprites.width, sprites.height), (512, 480));
        assert_eq!((font.width, font.height), (512, 8));
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

    /// Confirms the Bug atlas follows the original fourteen active frames.
    #[test]
    fn bug_frames_follow_the_original_spark_and_base_sequence() {
        let active = (0..14).map(bug_sprite).collect::<Vec<_>>();

        assert_eq!(
            active,
            vec![
                super::SpriteCell::new(8, 6),
                super::SpriteCell::new(9, 6),
                super::SpriteCell::new(10, 6),
                super::SpriteCell::new(11, 6),
                super::SpriteCell::new(12, 6),
                super::SpriteCell::new(11, 6),
                super::SpriteCell::new(10, 6),
                super::SpriteCell::new(11, 6),
                super::SpriteCell::new(12, 6),
                super::SpriteCell::new(11, 6),
                super::SpriteCell::new(10, 6),
                super::SpriteCell::new(9, 6),
                super::SpriteCell::new(8, 6),
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
}
