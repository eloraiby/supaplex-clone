//! SDL2 rendering from the original gameplay and front-end bitmap conversions.

use std::{error::Error, fmt, io::Cursor};

mod level;
use level::LevelRenderer;

use crate::platform as sdl2;
#[cfg(not(any(feature = "pocketgo", target_env = "uclibc")))]
use sdl2::render::ScaleMode;
use sdl2::{
    pixels::{Color, PixelFormatEnum},
    rect::Rect,
    render::{BlendMode, Canvas, Texture, TextureCreator},
    video::{Window, WindowContext},
};

use crate::actors::{
    Frame, Horizontal, enemy::EnemyPhase, explosion::ExplosionResidue, rounded::RoundedPhase,
};

use crate::{
    actors::{Actor, Direction, EnemyTurn, MurphyAnimation, State},
    assets::{
        self, AssetError, BACK_GRAPHICS_PATH, CONTROLS_GRAPHICS_PATH, FONT_GRAPHICS_PATH,
        GFX_TUTOR_GRAPHICS_PATH, MENU_FONT_GRAPHICS_PATH, MENU_GRAPHICS_PATH, PANEL_GRAPHICS_PATH,
        TITLE_GRAPHICS_PATH,
    },
    frontend::{ControlsTarget, MainMenuTarget},
    game::{BoardChange, Game, GameStatus},
    murphy_animation::{SourcePoint, SpritePart, sprite_parts},
};

/// Logical width used by the active display backend.
#[cfg(not(any(feature = "pocketgo", target_env = "uclibc")))]
pub const LOGICAL_WIDTH: u32 = 960;
#[cfg(any(feature = "pocketgo", target_env = "uclibc"))]
pub const LOGICAL_WIDTH: u32 = 320;

/// Logical height used by the active display backend.
#[cfg(not(any(feature = "pocketgo", target_env = "uclibc")))]
pub const LOGICAL_HEIGHT: u32 = 640;
#[cfg(any(feature = "pocketgo", target_env = "uclibc"))]
pub const LOGICAL_HEIGHT: u32 = 240;

/// Displayed height of the original 320×24 status panel.
#[cfg(not(any(feature = "pocketgo", target_env = "uclibc")))]
const HUD_HEIGHT: u32 = 72;
#[cfg(any(feature = "pocketgo", target_env = "uclibc"))]
const HUD_HEIGHT: u32 = 24;

/// Height of the scrolling board viewport above the HUD.
const VIEW_HEIGHT: u32 = LOGICAL_HEIGHT - HUD_HEIGHT;

/// Displayed width and height of one board cell.
#[cfg(not(any(feature = "pocketgo", target_env = "uclibc")))]
const TILE_SIZE: u32 = 32;
#[cfg(any(feature = "pocketgo", target_env = "uclibc"))]
const TILE_SIZE: u32 = 16;

/// Source width and height of one tile in the original fixed strip.
const FIXED_TILE_SIZE: u32 = 16;

/// Number of serialized, visible tiles stored consecutively in `FIXED.DAT`.
const FIXED_TILE_COUNT: u8 = 40;

/// Pixel width and height of one decoded font glyph.
const FONT_CELL_SIZE: u32 = 8;

/// Integer scale used to keep HUD lettering crisp and legible.
#[cfg(not(any(feature = "pocketgo", target_env = "uclibc")))]
const FONT_SCALE: u32 = 2;
#[cfg(any(feature = "pocketgo", target_env = "uclibc"))]
const FONT_SCALE: u32 = 1;

/// Integer enlargement used for original 320-pixel-wide screen coordinates.
const ORIGINAL_SCREEN_SCALE: u32 = LOGICAL_WIDTH / 320;

/// Three-times-scaled height of an original 320×200 front-end screen.
const ORIGINAL_SCREEN_HEIGHT: u32 = 200 * ORIGINAL_SCREEN_SCALE;

/// Centered top edge of a front-end screen inside the gameplay logical height.
const ORIGINAL_SCREEN_Y: i32 = ((LOGICAL_HEIGHT - ORIGINAL_SCREEN_HEIGHT) / 2) as i32;

/// Number of glyphs placed horizontally in `CHARS8.DAT`.
const FONT_GLYPHS: u8 = 64;

/// Palette-1 color index 4 used for the main menu's informational message.
const ORIGINAL_GREEN_TEXT: Color = Color::RGB(0x00, 0xb0, 0x60);

/// Palette-1 color index 6 used for active rows and zeroed panel counters.
const ORIGINAL_RED_TEXT: Color = Color::RGB(0xe0, 0x10, 0x10);

/// Palette-1 color index 8 used for inactive rows and ordinary panel values.
const ORIGINAL_BLUE_TEXT: Color = Color::RGB(0x70, 0x90, 0xe0);

/// Palette-1 color index 2 used for the first unfinished playable level.
const ORIGINAL_YELLOW_TEXT: Color = Color::RGB(0xe0, 0xe0, 0x00);

/// Visual progression category assigned to one main-menu level row.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MenuLevelStyle {
    /// First unfinished level, available to start and shown in yellow.
    Available,
    /// Previously solved level, available to replay and shown in green.
    Completed,
    /// Unfinished suffix level, unavailable and shown in red.
    Locked,
    /// Level advanced with a limited skip, available to replay and shown in blue.
    Skipped,
}

impl MenuLevelStyle {
    /// Maps one progression category to its original palette-1 text color.
    const fn color(self) -> Color {
        // These are expanded directly from the menu's original palette indices
        // 2, 4, 6, and 8 rather than invented selection-state colors.
        match self {
            Self::Available => ORIGINAL_YELLOW_TEXT,
            Self::Completed => ORIGINAL_GREEN_TEXT,
            Self::Locked => ORIGINAL_RED_TEXT,
            Self::Skipped => ORIGINAL_BLUE_TEXT,
        }
    }
}

/// One optional level-list row ready for original-coordinate menu rendering.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MenuLevelLine<'text> {
    /// One-based level number prefixed to the original title.
    pub number: usize,
    /// Borrowed decoded `LEVEL.LST` title.
    pub title: &'text str,
    /// Player-specific progress color and playability category.
    pub style: MenuLevelStyle,
}

/// Complete dynamic text and interaction state painted over `MENU.DAT`.
#[derive(Debug)]
pub struct MenuDisplay<'text> {
    /// Previous, current, and next visible level rows.
    pub levels: [Option<MenuLevelLine<'text>>; 3],
    /// Previous, current, and next visible player names.
    pub players: [Option<&'text str>; 3],
    /// Accumulated successful-play duration of the selected player.
    pub player_seconds: u64,
    /// Selected player's next unfinished level, absent after all are resolved.
    pub next_level: Option<usize>,
    /// Twenty-three-character status or prompt drawn across the center field.
    pub message: &'text str,
    /// Five preformatted rows in the currently visible ranking window.
    pub rankings: &'text [String],
    /// One-based number of the first ranking row shown in the window.
    pub ranking_position: usize,
    /// Up to ten preformatted hall-of-fame rows drawn in the upper-right field.
    pub hall_of_fame: &'text [String],
    /// Main-menu region under the mouse, used for responsive outline feedback.
    pub hovered: Option<MainMenuTarget>,
}

/// Integer enlargement from original 16-pixel tiles to the displayed board.
#[cfg(not(any(feature = "pocketgo", target_env = "uclibc")))]
const MOVING_SCALE: u32 = 2;
#[cfg(any(feature = "pocketgo", target_env = "uclibc"))]
const MOVING_SCALE: u32 = 1;

/// Display-space distance Murphy advances during one original movement update.
const MURPHY_STEP: i32 = 2 * MOVING_SCALE as i32;

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
    /// Convert black to zero alpha for modern overlay-only font rendering.
    Transparent,
}

/// Front-end textures and a persistent bitmap for the active level.
pub struct Renderer<'textures> {
    /// Persistent board bitmap updated in simulation callback order.
    level: LevelRenderer<'textures>,
    /// Original eight-pixel DOS font converted to an RGBA PNG.
    font: Texture<'textures>,
    /// Original 320×24 in-game panel, enlarged to the full logical width.
    panel: Texture<'textures>,
    /// Original 320×200 title artwork decoded with its dedicated palette.
    title: Texture<'textures>,
    /// Original 320×200 main-menu frame decoded with gameplay palette 1.
    menu: Texture<'textures>,
    /// Original 320×200 GFX tutorial decoded with gameplay palette 1.
    gfx_tutor: Texture<'textures>,
    /// Original 320×200 controls/options artwork decoded with palette 2.
    controls: Texture<'textures>,
    /// Original 320×200 information backdrop decoded with palette 0.
    back: Texture<'textures>,
    /// Original CHARS6 mask whose glyphs advance six source pixels in menus.
    menu_font: Texture<'textures>,
    /// Most recent camera centered on a live Murphy.
    ///
    /// A death transition replaces Murphy with an Explosion immediately. The
    /// retained camera keeps that blast visible instead of jumping to `(0, 0)`.
    camera: Camera,
}

impl<'textures> Renderer<'textures> {
    /// Loads production PNG assets and uploads nearest-neighbor SDL textures.
    pub fn new(
        texture_creator: &'textures TextureCreator<WindowContext>,
    ) -> Result<Self, RenderError> {
        // Asset acquisition is completed before the first texture upload so an
        // unbundled build reports missing files without retaining partial state.
        let graphics = assets::load_graphics().map_err(RenderError::Asset)?;
        let level = LevelRenderer::new(
            texture_creator,
            graphics.fixed.as_ref(),
            graphics.moving.as_ref(),
        )?;
        let font = load_texture(
            texture_creator,
            graphics.font.as_ref(),
            512,
            8,
            FONT_GRAPHICS_PATH,
            BlackPixelPolicy::Transparent,
        )?;
        let panel = load_texture(
            texture_creator,
            graphics.panel.as_ref(),
            320,
            24,
            PANEL_GRAPHICS_PATH,
            BlackPixelPolicy::Opaque,
        )?;
        let title = load_texture(
            texture_creator,
            graphics.title.as_ref(),
            320,
            200,
            TITLE_GRAPHICS_PATH,
            BlackPixelPolicy::Opaque,
        )?;
        let menu = load_texture(
            texture_creator,
            graphics.menu.as_ref(),
            320,
            200,
            MENU_GRAPHICS_PATH,
            BlackPixelPolicy::Opaque,
        )?;
        let gfx_tutor = load_texture(
            texture_creator,
            graphics.gfx_tutor.as_ref(),
            320,
            200,
            GFX_TUTOR_GRAPHICS_PATH,
            BlackPixelPolicy::Opaque,
        )?;
        let controls = load_texture(
            texture_creator,
            graphics.controls.as_ref(),
            320,
            200,
            CONTROLS_GRAPHICS_PATH,
            BlackPixelPolicy::Opaque,
        )?;
        let back = load_texture(
            texture_creator,
            graphics.back.as_ref(),
            320,
            200,
            BACK_GRAPHICS_PATH,
            BlackPixelPolicy::Opaque,
        )?;
        let menu_font = load_texture(
            texture_creator,
            graphics.menu_font.as_ref(),
            512,
            8,
            MENU_FONT_GRAPHICS_PATH,
            BlackPixelPolicy::Transparent,
        )?;

        Ok(Self {
            level,
            font,
            panel,
            title,
            menu,
            gfx_tutor,
            controls,
            back,
            menu_font,
            camera: Camera::default(),
        })
    }

    /// Draws the original title at exact three-times scale with black letterboxing.
    pub fn draw_splash(&mut self, canvas: &mut Canvas<Window>) -> Result<(), RenderError> {
        // A 320×200 image enlarged by three occupies 960×600. Centering those
        // pixels preserves its aspect ratio inside the taller gameplay window.
        canvas.set_draw_color(Color::RGB(0, 0, 0));
        canvas.clear();
        canvas
            .copy(
                &self.title,
                None,
                Rect::new(0, ORIGINAL_SCREEN_Y, LOGICAL_WIDTH, ORIGINAL_SCREEN_HEIGHT),
            )
            .map_err(RenderError::Sdl)
    }

    /// Draws the original main menu with live players, rankings, and level state.
    pub fn draw_menu(
        &mut self,
        canvas: &mut Canvas<Window>,
        display: &MenuDisplay<'_>,
    ) -> Result<(), RenderError> {
        // MENU.DAT supplies all borders, labels, arrows, and decorative controls.
        // Repainting it first also clears text left by the previous selection.
        canvas.set_draw_color(Color::RGB(0, 0, 0));
        canvas.clear();
        canvas
            .copy(
                &self.menu,
                None,
                Rect::new(0, ORIGINAL_SCREEN_Y, LOGICAL_WIDTH, ORIGINAL_SCREEN_HEIGHT),
            )
            .map_err(RenderError::Sdl)?;

        // Paint the three player rows in their original blue/red/blue order.
        // Missing rows remain empty rather than inventing placeholder profiles.
        let player_y = [155, 164, 173];
        for (row_index, player) in display.players.iter().enumerate() {
            if let Some(player) = player {
                let color = if row_index == 1 {
                    ORIGINAL_RED_TEXT
                } else {
                    ORIGINAL_BLUE_TEXT
                };
                self.draw_menu_text(canvas, player, 16, player_y[row_index], color)?;
            }
        }
        let current_player = display.players[1].unwrap_or("--------");
        self.draw_menu_text(canvas, current_player, 168, 93, ORIGINAL_BLUE_TEXT)?;
        self.draw_menu_text(
            canvas,
            &format_menu_time(display.player_seconds),
            224,
            93,
            ORIGINAL_BLUE_TEXT,
        )?;
        self.draw_menu_text(
            canvas,
            &display
                .next_level
                .map_or_else(|| "---".to_owned(), |level| format!("{level:03}")),
            288,
            93,
            ORIGINAL_BLUE_TEXT,
        )?;
        self.draw_menu_text(canvas, display.message, 168, 127, ORIGINAL_GREEN_TEXT)?;

        // Unlike a modern highlight, original level-row colors describe each
        // row's progression state regardless of which of the three is central.
        let level_y = [155, 164, 173];
        for (row_index, level) in display.levels.iter().enumerate() {
            if let Some(level) = level {
                self.draw_menu_text(
                    canvas,
                    &format_menu_level(level.number, level.title),
                    144,
                    level_y[row_index],
                    level.style.color(),
                )?;
            }
        }

        // Rankings center the third visible row in red, matching the source
        // list's scrolling window. Hall-of-fame rows remain uniformly blue.
        for (row_index, ranking) in display.rankings.iter().take(5).enumerate() {
            let color = if row_index == 2 {
                ORIGINAL_RED_TEXT
            } else {
                ORIGINAL_BLUE_TEXT
            };
            self.draw_menu_text(canvas, ranking, 8, 92 + row_index as i32 * 9, color)?;
        }
        self.draw_menu_text(
            canvas,
            &format!("{:02}", display.ranking_position.min(99)),
            144,
            110,
            ORIGINAL_RED_TEXT,
        )?;
        for (row_index, entry) in display.hall_of_fame.iter().take(10).enumerate() {
            self.draw_menu_text(
                canvas,
                entry,
                184,
                28 + row_index as i32 * 9,
                ORIGINAL_BLUE_TEXT,
            )?;
        }

        if let Some(target) = display.hovered {
            // A one-source-pixel yellow outline replaces the animated DOS border
            // and makes mouse selectability apparent at every window scale.
            draw_original_outline(canvas, target.original_bounds(), ORIGINAL_YELLOW_TEXT)?;
        }
        Ok(())
    }

    /// Draws the original illustrated actor and hardware GFX tutorial.
    pub fn draw_gfx_tutor(&mut self, canvas: &mut Canvas<Window>) -> Result<(), RenderError> {
        // The tutorial is a complete opaque screen, so the same centered DOS
        // screen blit used by the title and menu needs no additional overlays.
        draw_original_screen(canvas, &self.gfx_tutor)
    }

    /// Draws the original controls/options circuit-board background.
    pub fn draw_controls(&mut self, canvas: &mut Canvas<Window>) -> Result<(), RenderError> {
        // Interactive highlights are drawn by the caller after this immutable
        // background, allowing audio state to change without editing the asset.
        draw_original_screen(canvas, &self.controls)
    }

    /// Draws supported audio/input selections and the current controls hover.
    pub fn draw_controls_state(
        &mut self,
        canvas: &mut Canvas<Window>,
        music_enabled: bool,
        effects_enabled: bool,
        hovered: Option<ControlsTarget>,
    ) -> Result<(), RenderError> {
        // Green outlines expose the live settings this port can reproduce. The
        // keyboard remains selected because gameplay does not silently switch
        // to an unavailable joystick merely because its artwork was clicked.
        if music_enabled {
            draw_original_outline(
                canvas,
                ControlsTarget::Music.original_bounds(),
                ORIGINAL_GREEN_TEXT,
            )?;
        }
        if effects_enabled {
            draw_original_outline(
                canvas,
                ControlsTarget::Effects.original_bounds(),
                ORIGINAL_GREEN_TEXT,
            )?;
        }
        draw_original_outline(
            canvas,
            ControlsTarget::Keyboard.original_bounds(),
            ORIGINAL_GREEN_TEXT,
        )?;

        // Yellow has precedence over an active green outline while the mouse
        // is present, preserving obvious feedback on clickable controls.
        if let Some(target) = hovered {
            draw_original_outline(canvas, target.original_bounds(), ORIGINAL_YELLOW_TEXT)?;
        }
        Ok(())
    }

    /// Draws the original information backdrop and caller-supplied white text.
    pub fn draw_information(
        &mut self,
        canvas: &mut Canvas<Window>,
        lines: &[(&str, i32, i32)],
    ) -> Result<(), RenderError> {
        // BACK.DAT supplies the textured lower field used by Statistics and
        // Credits. Text coordinates remain in the original 320×200 space.
        draw_original_screen(canvas, &self.back)?;
        for &(text, x, y) in lines {
            self.draw_menu_text(canvas, text, x, y, Color::RGB(0xf0, 0xf0, 0xf0))?;
        }
        Ok(())
    }

    /// Covers the current logical frame with a blendable black fade layer.
    pub fn draw_black_overlay(
        &mut self,
        canvas: &mut Canvas<Window>,
        opacity: u8,
    ) -> Result<(), RenderError> {
        // SDL's canvas blend mode applies the alpha component to primitive fills.
        // Zero is skipped to avoid needless backend work on fully visible frames.
        if opacity == 0 {
            return Ok(());
        }
        canvas.set_blend_mode(BlendMode::Blend);
        canvas.set_draw_color(Color::RGBA(0, 0, 0, opacity));
        canvas
            .fill_rect(Rect::new(0, 0, LOGICAL_WIDTH, LOGICAL_HEIGHT))
            .map_err(RenderError::Sdl)
    }

    /// Presents the saved level image, HUD, and completion/death overlay.
    pub fn draw(
        &mut self,
        canvas: &mut Canvas<Window>,
        game: &Game,
        level_number: usize,
        steps_per_second: u32,
    ) -> Result<(), RenderError> {
        // Clear the display around the board and HUD. The saved level bitmap
        // survives this operation and changes only through simulation updates.
        canvas.set_draw_color(Color::RGB(0, 0, 0));
        canvas.clear();

        // A live Murphy supplies a fresh target every frame. Terminal snapshots
        // have no Murphy, so they deliberately retain the last playable view.
        if let Some(camera) = camera_for(game) {
            self.camera = camera;
        }
        let camera = self.camera;

        self.level.draw(canvas, camera)?;

        self.draw_hud(canvas, game, level_number, steps_per_second)?;
        self.draw_status_overlay(canvas, game.status())?;
        Ok(())
    }

    /// Initializes the saved board image before a new level's first fade or tick.
    pub fn begin_level(&mut self, game: &Game) -> Result<(), RenderError> {
        self.camera = camera_for(game).unwrap_or_default();
        self.level.reset(game.board())
    }

    /// Applies committed changes immediately after each simulation operation.
    pub fn apply_board_changes(&mut self, changes: &[BoardChange]) {
        self.level.apply(changes);
    }

    /// Draws the original panel and its live values at their historical positions.
    fn draw_hud(
        &mut self,
        canvas: &mut Canvas<Window>,
        game: &Game,
        level_number: usize,
        steps_per_second: u32,
    ) -> Result<(), RenderError> {
        // PANEL.DAT is exactly 320×24, so a three-times integer copy fills the
        // logical width without sampling artifacts or changing its proportions.
        let hud_y = VIEW_HEIGHT as i32;
        canvas
            .copy(
                &self.panel,
                None,
                Rect::new(0, hud_y, LOGICAL_WIDTH, HUD_HEIGHT),
            )
            .map_err(RenderError::Sdl)?;

        // Palette indices 6 and 8 are the original red highlight and blue
        // informational colors. The PNG font is a mask, so SDL color modulation
        // recreates those indexed writes over the preserved panel background.
        self.draw_panel_text(canvas, "MURPHY", 72, 3, ORIGINAL_RED_TEXT)?;
        self.draw_panel_text(
            canvas,
            &format!("{level_number:03}"),
            16,
            14,
            ORIGINAL_BLUE_TEXT,
        )?;
        self.draw_panel_text(canvas, game.title(), 64, 14, ORIGINAL_BLUE_TEXT)?;

        let infotrons = game.remaining_infotrons().min(999);
        let infotron_color = if infotrons == 0 {
            ORIGINAL_RED_TEXT
        } else {
            ORIGINAL_BLUE_TEXT
        };
        self.draw_panel_text(canvas, &format!("{infotrons:03}"), 272, 14, infotron_color)?;

        // The original panel shows only the last two digits for each time field
        // and Red Disk inventory. Derive elapsed play time from fixed simulation
        // ticks so menus, pauses, and slow render frames do not inflate it.
        let total_seconds = game.tick_count() / u64::from(steps_per_second.max(1));
        let seconds = total_seconds % 60;
        let minutes = total_seconds / 60 % 60;
        let hours = total_seconds / 3_600 % 100;
        self.draw_panel_text(canvas, &format!("{hours:02}"), 160, 3, ORIGINAL_RED_TEXT)?;
        self.draw_panel_text(canvas, &format!("{minutes:02}"), 184, 3, ORIGINAL_RED_TEXT)?;
        self.draw_panel_text(canvas, &format!("{seconds:02}"), 208, 3, ORIGINAL_RED_TEXT)?;

        let red_disks = game.red_disks() % 100;
        let red_disk_color = if red_disks == 0 {
            ORIGINAL_BLUE_TEXT
        } else {
            ORIGINAL_RED_TEXT
        };
        self.draw_panel_text(canvas, &format!("{red_disks:02}"), 304, 14, red_disk_color)
    }

    /// Draws CHARS8 text using original panel coordinates and three-times scale.
    fn draw_panel_text(
        &mut self,
        canvas: &mut Canvas<Window>,
        text: &str,
        original_x: i32,
        original_y: i32,
        color: Color,
    ) -> Result<(), RenderError> {
        // Converting both axes at this boundary keeps every call site readable
        // against the original drawing routine's literal 320×24 coordinates.
        self.draw_text_scaled(
            canvas,
            text,
            original_x * ORIGINAL_SCREEN_SCALE as i32,
            VIEW_HEIGHT as i32 + original_y * ORIGINAL_SCREEN_SCALE as i32,
            color,
            ORIGINAL_SCREEN_SCALE,
        )
    }

    /// Draws CHARS6 text at an original 320×200 menu coordinate.
    fn draw_menu_text(
        &mut self,
        canvas: &mut Canvas<Window>,
        text: &str,
        original_x: i32,
        original_y: i32,
        color: Color,
    ) -> Result<(), RenderError> {
        // CHARS6 stores each glyph in an eight-bit slot but the original loop
        // copies and advances only six pixels. Cropping the source rectangle
        // preserves that compact spacing rather than overlapping eight-pixel cells.
        self.menu_font.set_color_mod(color.r, color.g, color.b);
        let scale = ORIGINAL_SCREEN_SCALE;
        let glyph_advance = 6 * scale;
        for (character_index, character) in text.chars().enumerate() {
            let character = character.to_ascii_uppercase();
            let ascii = u32::from(character);
            let glyph = if (32..32 + u32::from(FONT_GLYPHS)).contains(&ascii) {
                ascii - 32
            } else {
                u32::from(b'?' - b' ')
            };
            let source = Rect::new((glyph * FONT_CELL_SIZE) as i32, 0, 6, FONT_CELL_SIZE);
            let destination = Rect::new(
                original_x * scale as i32 + character_index as i32 * glyph_advance as i32,
                ORIGINAL_SCREEN_Y + original_y * scale as i32,
                6 * scale,
                FONT_CELL_SIZE * scale,
            );
            canvas
                .copy(&self.menu_font, source, destination)
                .map_err(RenderError::Sdl)?;
        }
        Ok(())
    }

    /// Draws a centered terminal-state banner while keeping the board visible.
    fn draw_status_overlay(
        &mut self,
        canvas: &mut Canvas<Window>,
        status: GameStatus,
    ) -> Result<(), RenderError> {
        #[cfg(not(any(feature = "pocketgo", target_env = "uclibc")))]
        let restart_key = "R";
        #[cfg(any(feature = "pocketgo", target_env = "uclibc"))]
        let restart_key = "X";
        let (message, color) = match status {
            GameStatus::Playing => return Ok(()),
            GameStatus::Completed => (
                format!("LEVEL COMPLETE - PRESS {restart_key}"),
                Color::RGB(80, 255, 120),
            ),
            GameStatus::Dead => (
                format!("MURPHY DESTROYED - PRESS {restart_key}"),
                Color::RGB(255, 90, 70),
            ),
        };

        let width = text_width(&message) + 32;
        let x = (LOGICAL_WIDTH.saturating_sub(width) / 2) as i32;
        let y = (VIEW_HEIGHT / 2).saturating_sub(24) as i32;
        canvas.set_draw_color(Color::RGB(20, 20, 24));
        canvas
            .fill_rect(Rect::new(x, y, width, 48))
            .map_err(RenderError::Sdl)?;
        self.draw_text(canvas, &message, x + 16, y + 16, color)
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
        // General overlays retain the clone's existing two-times font size;
        // original-resolution UI surfaces call the scaled primitive directly.
        self.draw_text_scaled(canvas, text, x, y, color, FONT_SCALE)
    }

    /// Draws supported ASCII text with an explicit integer glyph scale.
    fn draw_text_scaled(
        &mut self,
        canvas: &mut Canvas<Window>,
        text: &str,
        x: i32,
        y: i32,
        color: Color,
        scale: u32,
    ) -> Result<(), RenderError> {
        // Color modulation turns the white mask into one indexed-palette color;
        // transparent black leaves the destination panel or board untouched.
        self.font.set_color_mod(color.r, color.g, color.b);
        let glyph_size = (FONT_CELL_SIZE * scale) as i32;

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
                FONT_CELL_SIZE * scale,
                FONT_CELL_SIZE * scale,
            );
            canvas
                .copy(&self.font, source, destination)
                .map_err(RenderError::Sdl)?;
        }

        Ok(())
    }
}

/// Copies one opaque 320×200 DOS screen into the centered logical viewport.
fn draw_original_screen(
    canvas: &mut Canvas<Window>,
    texture: &Texture<'_>,
) -> Result<(), RenderError> {
    // Clear the twenty-pixel logical bars above and below the three-times
    // enlarged artwork before copying the full texture without source cropping.
    canvas.set_draw_color(Color::RGB(0, 0, 0));
    canvas.clear();
    canvas
        .copy(
            texture,
            None,
            Rect::new(0, ORIGINAL_SCREEN_Y, LOGICAL_WIDTH, ORIGINAL_SCREEN_HEIGHT),
        )
        .map_err(RenderError::Sdl)
}

/// Draws one scaled outline over an original-coordinate front-end control.
fn draw_original_outline(
    canvas: &mut Canvas<Window>,
    bounds: (i32, i32, u32, u32),
    color: Color,
) -> Result<(), RenderError> {
    // Converting origin and size together preserves the inclusive source box;
    // the three-logical-pixel stroke matches one original screen pixel.
    let (x, y, width, height) = bounds;
    canvas.set_draw_color(color);
    for inset in 0..ORIGINAL_SCREEN_SCALE {
        let width = width
            .saturating_mul(ORIGINAL_SCREEN_SCALE)
            .saturating_sub(inset * 2);
        let height = height
            .saturating_mul(ORIGINAL_SCREEN_SCALE)
            .saturating_sub(inset * 2);
        if width == 0 || height == 0 {
            break;
        }
        canvas
            .draw_rect(Rect::new(
                x * ORIGINAL_SCREEN_SCALE as i32 + inset as i32,
                ORIGINAL_SCREEN_Y + y * ORIGINAL_SCREEN_SCALE as i32 + inset as i32,
                width,
                height,
            ))
            .map_err(RenderError::Sdl)?;
    }
    Ok(())
}

/// Formats accumulated player time as the original three-digit hour counter.
fn format_menu_time(total_seconds: u64) -> String {
    // Saturating the visible hours at 999 keeps the fixed ten-character field
    // intact while preserving minutes and seconds modulo their clock ranges.
    let hours = (total_seconds / 3_600).min(999);
    let minutes = total_seconds / 60 % 60;
    let seconds = total_seconds % 60;
    format!("{hours:03}:{minutes:02}:{seconds:02}")
}

/// Formats one original level-list row without exceeding its 29-character field.
fn format_menu_level(number: usize, title: &str) -> String {
    // Original titles are ASCII and at most 23 characters, but the truncation
    // also keeps custom level data inside the right-hand menu frame.
    format!("{number:03} {title}").chars().take(29).collect()
}

/// Selects the zonk fall or roll artwork from its bounded physical phase.
fn zonk_sprite_part(phase: RoundedPhase) -> Option<SpritePart> {
    match phase {
        RoundedPhase::Falling(frame) => Some(SpritePart {
            source: SourcePoint { x: 224, y: 82 },
            width: 16,
            height: 18,
            offset_x: 0,
            offset_y: -16 + i32::from(frame.index()) * 2,
        }),
        RoundedPhase::Rolling { direction, frame } => {
            let frame = frame.index();
            Some({
                let source_y = if direction == Horizontal::Left {
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
                    offset_x: if direction == Horizontal::Right {
                        -16
                    } else {
                        0
                    },
                    offset_y: 0,
                }
            })
        }
        RoundedPhase::Resting
        | RoundedPhase::Momentum
        | RoundedPhase::AwaitingFall
        | RoundedPhase::PreparingRoll(_)
        | RoundedPhase::Held => None,
    }
}

/// Selects the infotron fall or roll artwork from its bounded physical phase.
fn infotron_sprite_part(phase: RoundedPhase) -> Option<SpritePart> {
    match phase {
        RoundedPhase::Falling(frame) => Some(SpritePart {
            source: SourcePoint { x: 240, y: 178 },
            width: 16,
            height: 18,
            offset_x: 0,
            offset_y: -16 + i32::from(frame.index()) * 2,
        }),
        RoundedPhase::Rolling { direction, frame } => {
            let frame = frame.index();
            Some({
                // Frame four of the left strip really begins at x=8 in the
                // original pointer table. Preserve that historical coordinate.
                const LEFT_X: [i32; 8] = [0, 32, 64, 96, 8, 160, 192, 224];
                let (source_x, source_y) = if direction == Horizontal::Left {
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
                    offset_x: if direction == Horizontal::Right {
                        -16
                    } else {
                        0
                    },
                    offset_y: 0,
                }
            })
        }
        RoundedPhase::Resting
        | RoundedPhase::Momentum
        | RoundedPhase::AwaitingFall
        | RoundedPhase::PreparingRoll(_)
        | RoundedPhase::Held => None,
    }
}

/// Selects the source-retained Orange Disk fall using an eight-picture frame.
fn orange_sprite_part(frame: Frame<8>) -> SpritePart {
    SpritePart {
        source: SourcePoint { x: 128, y: 64 },
        width: 16,
        height: 18,
        offset_x: 0,
        offset_y: i32::from(frame.index()) * 2,
    }
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
fn snik_snak_sprite_part(phase: EnemyPhase) -> SpritePart {
    let frame = match phase {
        EnemyPhase::Turning { frame, .. } | EnemyPhase::Moving { frame, .. } => frame.index(),
    };
    let (source_index, width, height, offset_x, offset_y) = match phase {
        EnemyPhase::Turning { turn, .. } => {
            let cycle_start = match turn {
                EnemyTurn::Left => 0,
                EnemyTurn::Right => 8,
            };
            (cycle_start + usize::from(frame), 16, 16, 0, 0)
        }
        EnemyPhase::Moving {
            direction: Direction::Up,
            ..
        } => {
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
        EnemyPhase::Moving {
            direction: Direction::Left,
            ..
        } => (24 + usize::from(frame), 32, 16, 0, 0),
        EnemyPhase::Moving {
            direction: Direction::Down,
            ..
        } => (
            32 + usize::from(frame),
            16,
            18,
            0,
            -16 + i32::from(frame) * 2,
        ),
        EnemyPhase::Moving {
            direction: Direction::Right,
            ..
        } => (40 + usize::from(frame), 32, 16, -16, 0),
    };
    SpritePart {
        source: SNIK_SNAK_SOURCE_POINTS[source_index],
        width,
        height,
        offset_x,
        offset_y,
    }
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
fn electron_sprite_part(phase: EnemyPhase) -> SpritePart {
    let frame = match phase {
        EnemyPhase::Turning { frame, .. } | EnemyPhase::Moving { frame, .. } => frame.index(),
    };
    let (source_index, width, height, offset_x, offset_y) = match phase {
        EnemyPhase::Turning { turn, .. } => {
            let cycle_start = match turn {
                EnemyTurn::Left => 0,
                EnemyTurn::Right => 8,
            };
            (cycle_start + usize::from(frame), 16, 16, 0, 0)
        }
        EnemyPhase::Moving {
            direction: Direction::Up,
            ..
        } => (
            16 + usize::from(frame),
            16,
            18,
            0,
            14 - i32::from(frame) * 2,
        ),
        EnemyPhase::Moving {
            direction: Direction::Left,
            ..
        } => (24 + usize::from(frame), 32, 16, 0, 0),
        EnemyPhase::Moving {
            direction: Direction::Down,
            ..
        } => (
            32 + usize::from(frame),
            16,
            18,
            0,
            -16 + i32::from(frame) * 2,
        ),
        EnemyPhase::Moving {
            direction: Direction::Right,
            ..
        } => (40 + usize::from(frame), 32, 16, -16, 0),
    };
    SpritePart {
        source: ELECTRON_SOURCE_POINTS[source_index],
        width,
        height,
        offset_x,
        offset_y,
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

/// Loads one RGBA PNG, applies its requested black-pixel policy, and uploads it.
fn load_texture<'textures>(
    texture_creator: &'textures TextureCreator<WindowContext>,
    png_bytes: &[u8],
    expected_width: u32,
    expected_height: u32,
    asset_name: &'static str,
    black_pixel_policy: BlackPixelPolicy,
) -> Result<Texture<'textures>, RenderError> {
    let mut image = decode_sized_png(png_bytes, expected_width, expected_height, asset_name)?;
    // Front-end artwork is opaque and fonts use their selected color key.
    // The level renderer loads both gameplay atlases as opaque bitmap sources.
    apply_black_pixel_policy(&mut image.pixels, black_pixel_policy);

    let blend_mode = match black_pixel_policy {
        BlackPixelPolicy::Opaque => BlendMode::None,
        BlackPixelPolicy::Transparent => BlendMode::Blend,
    };
    upload_texture(texture_creator, &image, blend_mode)
}

/// Validates atlas dimensions before descriptors can address decoded pixels.
fn decode_sized_png(
    png_bytes: &[u8],
    expected_width: u32,
    expected_height: u32,
    asset_name: &'static str,
) -> Result<DecodedPng, RenderError> {
    // Validate the decoded geometry before applying any pixel transformation so
    // malformed production resources report their dimensions without mutation.
    let image = decode_png(png_bytes)?;
    if image.width != expected_width || image.height != expected_height {
        return Err(RenderError::UnexpectedDimensions {
            asset: asset_name,
            expected_width,
            expected_height,
            actual_width: image.width,
            actual_height: image.height,
        });
    }

    Ok(image)
}

/// Uploads prepared pixels with an explicit coverage rule and nearest sampling.
fn upload_texture<'textures>(
    texture_creator: &'textures TextureCreator<WindowContext>,
    image: &DecodedPng,
    blend_mode: BlendMode,
) -> Result<Texture<'textures>, RenderError> {
    // Upload the transformed straight-alpha bytes. The desktop texture mode is
    // set explicitly rather than relying only on SDL_RENDER_SCALE_QUALITY: an
    // environment override or backend default must not introduce atlas bleed.
    let mut texture = texture_creator
        .create_texture_streaming(PixelFormatEnum::RGBA32, image.width, image.height)
        .map_err(|error| RenderError::Sdl(error.to_string()))?;
    texture
        .update(None, &image.pixels, image.width as usize * 4)
        .map_err(|error| RenderError::Sdl(error.to_string()))?;
    #[cfg(not(any(feature = "pocketgo", target_env = "uclibc")))]
    texture.set_scale_mode(ScaleMode::Nearest);
    texture.set_blend_mode(blend_mode);
    Ok(texture)
}

/// Applies opaque-copy or black-colorkey semantics to tightly packed RGBA pixels.
fn apply_black_pixel_policy(pixels: &mut [u8], policy: BlackPixelPolicy) {
    let (pixels, remainder) = pixels.as_chunks_mut::<4>();
    debug_assert!(
        remainder.is_empty(),
        "RGBA image must contain complete pixels"
    );

    match policy {
        // A DOS bitmap copy has no alpha channel. Force every source pixel to
        // replace its destination, even if an unbundled PNG was previously
        // converted with transparent black or contains partial alpha.
        BlackPixelPolicy::Opaque => {
            for pixel in pixels {
                pixel[3] = u8::MAX;
            }
        }
        // Transparent images use pure black as their colorkey. Colored pixels
        // keep their original alpha for deliberately translucent artwork.
        BlackPixelPolicy::Transparent => {
            for pixel in pixels {
                if pixel[0] == 0 && pixel[1] == 0 && pixel[2] == 0 {
                    pixel[3] = 0;
                }
            }
        }
    }
}

/// Decodes one production PNG and requires a tightly packed RGBA8 output frame.
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

/// Returns the direct FIXED.DAT source rectangle for one serialized tile code.
fn fixed_tile_source(tile: u8) -> Rect {
    // Invisible Wall is code forty and has no picture. Callers skip that actor,
    // while malformed larger codes defensively select the black Space tile.
    let tile = if tile < FIXED_TILE_COUNT { tile } else { 0 };
    Rect::new(
        i32::from(tile) * FIXED_TILE_SIZE as i32,
        0,
        FIXED_TILE_SIZE,
        FIXED_TILE_SIZE,
    )
}

/// Selects a lethal Bug picture; out-of-range table indices are unrepresentable.
fn bug_sprite_part(frame: Frame<14>) -> SpritePart {
    const BUG: [SourcePoint; 14] = [
        SourcePoint { x: 304, y: 100 },
        SourcePoint { x: 256, y: 196 },
        SourcePoint { x: 272, y: 196 },
        SourcePoint { x: 288, y: 196 },
        SourcePoint { x: 304, y: 196 },
        SourcePoint { x: 288, y: 196 },
        SourcePoint { x: 272, y: 196 },
        SourcePoint { x: 288, y: 196 },
        SourcePoint { x: 304, y: 196 },
        SourcePoint { x: 288, y: 196 },
        SourcePoint { x: 272, y: 196 },
        SourcePoint { x: 256, y: 196 },
        SourcePoint { x: 304, y: 100 },
        SourcePoint { x: 304, y: 64 },
    ];
    cell_sprite_part(BUG[usize::from(frame.index())])
}

/// Selects one of the two explosion strips with bounded explosion progress.
fn explosion_sprite_part(residue: ExplosionResidue, frame: Frame<8>) -> SpritePart {
    let start_x = match residue {
        ExplosionResidue::Empty => 0,
        ExplosionResidue::Infotron => 128,
    };
    cell_sprite_part(SourcePoint {
        x: start_x + i32::from(frame.index()) * 16,
        y: 196,
    })
}

/// Wraps a source coordinate in the shared opaque one-cell drawing geometry.
fn cell_sprite_part(source: SourcePoint) -> SpritePart {
    SpritePart {
        source,
        width: FIXED_TILE_SIZE,
        height: FIXED_TILE_SIZE,
        offset_x: 0,
        offset_y: 0,
    }
}

/// Maps one Terminal display scanline to its retained FIXED.DAT source row.
fn terminal_source_row(frame: u8, destination_row: u8) -> u8 {
    // The initial tile exposes rows two through nine. After its first scroll,
    // the seven-line pattern in source rows three through nine rotates upward
    // and repeats its new top row in the eighth display scanline. This literal
    // mapping reproduces all seven historical terminal pictures from FIXED.DAT.
    debug_assert!((2..=9).contains(&destination_row));
    let phase = frame % 7;
    if phase == 0 {
        return destination_row;
    }
    3 + (destination_row - 2 + phase - 1) % 7
}

/// Returns camera interpolation from a concrete moving actor's bounded progress.
fn movement_offset(state: &State) -> (i32, i32) {
    let rounded = match state.actor() {
        Actor::Murphy(actor) => {
            return match actor.sprite_pose() {
                Some((action, frame)) => murphy_movement_offset(action, frame),
                None => (0, 0),
            };
        }
        Actor::Zonk(actor) => actor.phase(),
        Actor::Infotron(actor) => actor.phase(),
        _ => return (0, 0),
    };
    match rounded {
        RoundedPhase::Falling(frame) => (0, -remaining_distance(frame)),
        RoundedPhase::Rolling { direction, frame } => {
            let distance = remaining_distance(frame);
            match direction {
                Horizontal::Left => (distance, -distance),
                Horizontal::Right => (-distance, -distance),
            }
        }
        RoundedPhase::Resting
        | RoundedPhase::Momentum
        | RoundedPhase::AwaitingFall
        | RoundedPhase::PreparingRoll(_)
        | RoundedPhase::Held => (0, 0),
    }
}

/// Converts the remaining portion of an eight-picture transfer to logical pixels.
fn remaining_distance(frame: Frame<8>) -> i32 {
    ((1.0 - f32::from(frame.index()) / 7.0) * TILE_SIZE as f32).round() as i32
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
    /// A required production asset could not be acquired.
    Asset(AssetError),
    /// The PNG decoder rejected a production asset.
    Png(png::DecodingError),
    /// Decoded dimensions did not match the asset contract.
    UnexpectedDimensions {
        /// Human-readable production asset name.
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
    /// Gameplay drawing was requested before its level bitmap was initialized.
    LevelNotInitialized,
    /// SDL rejected texture creation, upload, or a draw operation.
    Sdl(String),
}

impl fmt::Display for RenderError {
    /// Formats an actionable asset or SDL failure.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Asset(error) => error.fmt(formatter),
            Self::Png(error) => write!(formatter, "could not decode production PNG: {error}"),
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
                "production PNG decoded as {color_type:?}/{bit_depth:?}; expected RGBA/8-bit"
            ),
            Self::LevelNotInitialized => {
                formatter.write_str("begin_level must precede gameplay drawing")
            }
            Self::Sdl(error) => write!(formatter, "SDL rendering failed: {error}"),
        }
    }
}

impl Error for RenderError {
    /// Exposes the PNG decoder source while string-based SDL errors have none.
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Asset(error) => Some(error),
            Self::Png(error) => Some(error),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    //! Pure mapping and decoding tests that do not initialize SDL video.

    use super::{
        BlackPixelPolicy, FIXED_TILE_COUNT, FIXED_TILE_SIZE, MOVING_SCALE, SourcePoint,
        apply_black_pixel_policy, bug_sprite_part, decode_png, electron_sprite_part,
        explosion_sprite_part, fixed_tile_source, infotron_sprite_part, murphy_movement_offset,
        orange_sprite_part, snik_snak_sprite_part, terminal_source_row, zonk_sprite_part,
    };
    use crate::actors::{
        Frame, Horizontal, enemy::EnemyPhase, explosion::ExplosionResidue, rounded::RoundedPhase,
    };
    use crate::{
        actors::{Direction, EnemyTurn, MurphyAnimation, MurphyMoveTarget, MurphyPushTarget},
        assets,
    };

    /// Confirms all production resources decode to their contracted RGBA sizes.
    #[test]
    fn production_render_assets_are_valid_rgba_pngs() {
        let graphics = assets::load_graphics().expect("production graphics should load");
        let fixed =
            decode_png(graphics.fixed.as_ref()).expect("original fixed strip should decode");
        let moving =
            decode_png(graphics.moving.as_ref()).expect("original moving sheet should decode");
        let font = decode_png(graphics.font.as_ref()).expect("font should decode");

        assert_eq!((fixed.width, fixed.height), (640, 16));
        assert_eq!((moving.width, moving.height), (320, 462));
        assert_eq!((font.width, font.height), (512, 8));
    }

    /// Confirms Terminal phases reconstruct the original seven-picture row cycle.
    #[test]
    fn terminal_scroll_frames_use_fixed_display_scanlines() {
        let frames = (0..7)
            .map(|frame| {
                (2..=9)
                    .map(|row| terminal_source_row(frame, row))
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();

        assert_eq!(
            frames,
            vec![
                vec![2, 3, 4, 5, 6, 7, 8, 9],
                vec![3, 4, 5, 6, 7, 8, 9, 3],
                vec![4, 5, 6, 7, 8, 9, 3, 4],
                vec![5, 6, 7, 8, 9, 3, 4, 5],
                vec![6, 7, 8, 9, 3, 4, 5, 6],
                vec![7, 8, 9, 3, 4, 5, 6, 7],
                vec![8, 9, 3, 4, 5, 6, 7, 8],
            ]
        );
    }

    /// Confirms opaque DOS copies and transparent overlays treat black differently.
    #[test]
    fn black_pixel_policy_preserves_moving_erase_pixels() {
        let source = [0, 0, 0, 0, 12, 34, 56, 192];
        let mut opaque = source;
        let mut transparent = source;

        // Solid moving frames force every alpha byte to full coverage, while
        // overlay textures discard only pure black and retain colored alpha.
        apply_black_pixel_policy(&mut opaque, BlackPixelPolicy::Opaque);
        apply_black_pixel_policy(&mut transparent, BlackPixelPolicy::Transparent);

        assert_eq!(opaque, [0, 0, 0, 255, 12, 34, 56, 255]);
        assert_eq!(transparent, [0, 0, 0, 0, 12, 34, 56, 192]);
    }

    /// Confirms every completed Base-eating rectangle carries opaque black background pixels.
    #[test]
    fn final_base_movement_frames_contain_opaque_erase_data() {
        let graphics = assets::load_graphics().expect("production graphics should load");
        let mut moving =
            decode_png(graphics.moving.as_ref()).expect("MOVING.DAT conversion should decode");
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

    /// Confirms every visible serialized tile maps directly inside FIXED.DAT.
    #[test]
    fn every_static_tile_mapping_stays_inside_fixed_strip() {
        for tile in 0..FIXED_TILE_COUNT {
            let source = fixed_tile_source(tile);

            assert_eq!(source.x(), i32::from(tile) * FIXED_TILE_SIZE as i32);
            assert_eq!(source.y(), 0);
            assert_eq!(source.width(), FIXED_TILE_SIZE);
            assert_eq!(source.height(), FIXED_TILE_SIZE);
            assert!(source.right() <= 640, "tile {tile} escaped FIXED.DAT");
        }

        // Invisible Wall and malformed codes deliberately fall back to Space;
        // neither may address a nonexistent forty-first source tile.
        assert_eq!(fixed_tile_source(40), fixed_tile_source(0));
        assert_eq!(fixed_tile_source(u8::MAX), fixed_tile_source(0));
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

        assert_eq!(
            murphy_movement_offset(movement, 0),
            (-14 * MOVING_SCALE as i32, 0)
        );
        assert_eq!(murphy_movement_offset(movement, 7), (0, 0));
        assert_eq!(
            next_started_world_x - completed_world_x,
            2 * MOVING_SCALE as i32
        );
    }

    /// Confirms every cardinal move uses the original two-pixel unscaled velocity.
    #[test]
    fn murphy_move_camera_offsets_advance_four_display_pixels_per_update() {
        let distance = 14 * MOVING_SCALE as i32;
        let cases = [
            (Direction::Up, (0, distance), (0, 0)),
            (Direction::Right, (-distance, 0), (0, 0)),
            (Direction::Down, (0, -distance), (0, 0)),
            (Direction::Left, (distance, 0), (0, 0)),
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

        let scale = MOVING_SCALE as i32;
        assert_eq!(murphy_movement_offset(push, 0), (-2 * scale, 0));
        assert_eq!(murphy_movement_offset(push, 7), (-16 * scale, 0));
        assert_eq!(murphy_movement_offset(port, 0), (0, 4 * scale));
        assert_eq!(murphy_movement_offset(port, 7), (0, 32 * scale));
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
        let active = (0..14)
            .map(|frame| bug_sprite_part(Frame::new(frame).unwrap()).source)
            .collect::<Vec<_>>();

        assert_eq!(
            active,
            vec![
                SourcePoint { x: 304, y: 100 },
                SourcePoint { x: 256, y: 196 },
                SourcePoint { x: 272, y: 196 },
                SourcePoint { x: 288, y: 196 },
                SourcePoint { x: 304, y: 196 },
                SourcePoint { x: 288, y: 196 },
                SourcePoint { x: 272, y: 196 },
                SourcePoint { x: 288, y: 196 },
                SourcePoint { x: 304, y: 196 },
                SourcePoint { x: 288, y: 196 },
                SourcePoint { x: 272, y: 196 },
                SourcePoint { x: 256, y: 196 },
                SourcePoint { x: 304, y: 100 },
                SourcePoint { x: 304, y: 64 },
            ]
        );
    }

    /// Confirms regular and Electron explosions use adjacent original MOVING.DAT strips.
    #[test]
    fn explosion_frames_use_literal_moving_coordinates() {
        let regular = (0..8)
            .map(|frame| {
                explosion_sprite_part(ExplosionResidue::Empty, Frame::new(frame).unwrap()).source
            })
            .collect::<Vec<_>>();
        let electron = (0..8)
            .map(|frame| {
                explosion_sprite_part(ExplosionResidue::Infotron, Frame::new(frame).unwrap()).source
            })
            .collect::<Vec<_>>();

        // Both families occupy y=196 and advance by one original 16-pixel tile;
        // the Infotron-producing family begins immediately after the regular one.
        assert_eq!(regular.first(), Some(&SourcePoint { x: 0, y: 196 }));
        assert_eq!(regular.last(), Some(&SourcePoint { x: 112, y: 196 }));
        assert_eq!(electron.first(), Some(&SourcePoint { x: 128, y: 196 }));
        assert_eq!(electron.last(), Some(&SourcePoint { x: 240, y: 196 }));
    }

    /// Confirms horizontal rolls use literal MOVING.DAT direction strips.
    #[test]
    fn rolling_frames_select_original_source_rows() {
        let zonk_left = zonk_sprite_part(RoundedPhase::Rolling {
            direction: Horizontal::Left,
            frame: Frame::new(3).unwrap(),
        })
        .expect("moving phase has a sprite");
        let zonk_right = zonk_sprite_part(RoundedPhase::Rolling {
            direction: Horizontal::Right,
            frame: Frame::new(3).unwrap(),
        })
        .expect("moving phase has a sprite");
        let infotron_left = infotron_sprite_part(RoundedPhase::Rolling {
            direction: Horizontal::Left,
            frame: Frame::new(4).unwrap(),
        })
        .expect("moving phase has a sprite");
        let infotron_right = infotron_sprite_part(RoundedPhase::Rolling {
            direction: Horizontal::Right,
            frame: Frame::new(4).unwrap(),
        })
        .expect("moving phase has a sprite");

        assert_eq!(zonk_left.source, SourcePoint { x: 96, y: 84 });
        assert_eq!(zonk_right.source, SourcePoint { x: 96, y: 100 });
        assert_eq!(infotron_left.source, SourcePoint { x: 8, y: 164 });
        assert_eq!(infotron_right.source, SourcePoint { x: 128, y: 180 });
    }

    /// Confirms falling actors use the original two-pixel gravity increments.
    #[test]
    fn gravity_frames_stop_two_pixels_before_the_destination_tile() {
        let first = zonk_sprite_part(RoundedPhase::Falling(Frame::new(0).unwrap()))
            .expect("moving phase has a sprite");
        let last = zonk_sprite_part(RoundedPhase::Falling(Frame::new(7).unwrap()))
            .expect("moving phase has a sprite");

        assert_eq!(first.offset_y, -16);
        assert_eq!(last.offset_y, -2);
        assert_eq!((first.source.x, first.source.y), (224, 82));
    }

    /// Confirms each gravity actor selects its own unscaled source picture.
    #[test]
    fn falling_actor_sources_remain_distinct() {
        let infotron = infotron_sprite_part(RoundedPhase::Falling(Frame::new(0).unwrap()))
            .expect("moving phase has a sprite");
        let orange = orange_sprite_part(Frame::new(0).unwrap());

        assert_eq!((infotron.source.x, infotron.source.y), (240, 178));
        assert_eq!((orange.source.x, orange.source.y), (128, 64));
        assert_eq!(orange.offset_y, 0);
    }

    /// Confirms Snik Snak turns and moves use the literal MOVING.DAT rectangles.
    #[test]
    fn snik_snak_frames_preserve_turn_order_and_wide_horizontal_composites() {
        let turn = snik_snak_sprite_part(EnemyPhase::Turning {
            turn: EnemyTurn::Left,
            frame: Frame::new(2).unwrap(),
        });
        let move_left = snik_snak_sprite_part(EnemyPhase::Moving {
            direction: Direction::Left,
            frame: Frame::new(7).unwrap(),
        });
        let move_up = snik_snak_sprite_part(EnemyPhase::Moving {
            direction: Direction::Up,
            frame: Frame::new(0).unwrap(),
        });

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
        let right_turn = electron_sprite_part(EnemyPhase::Turning {
            turn: EnemyTurn::Right,
            frame: Frame::new(1).unwrap(),
        });
        let down_five = electron_sprite_part(EnemyPhase::Moving {
            direction: Direction::Down,
            frame: Frame::new(5).unwrap(),
        });
        let move_right = electron_sprite_part(EnemyPhase::Moving {
            direction: Direction::Right,
            frame: Frame::new(1).unwrap(),
        });

        assert_eq!((right_turn.source.x, right_turn.source.y), (112, 404));
        assert_eq!((down_five.source.x, down_five.source.y), (80, 403));
        assert_eq!((down_five.width, down_five.height), (16, 18));
        assert_eq!((move_right.width, move_right.offset_x), (32, -16));
    }

    /// Captures every original gravity, enemy, Bug, and explosion sprite rectangle.
    #[test]
    fn all_sprite_rectangles_preserve_original_coordinates() {
        let mut digest = 0xcbf29ce484222325_u64;
        let mut record = |part: crate::murphy_animation::SpritePart| {
            // Explicit signed little-endian fields avoid layout and pointer dependencies.
            for value in [
                i64::from(part.source.x),
                i64::from(part.source.y),
                i64::from(part.width),
                i64::from(part.height),
                i64::from(part.offset_x),
                i64::from(part.offset_y),
            ] {
                for byte in value.to_le_bytes() {
                    digest = (digest ^ u64::from(byte)).wrapping_mul(0x100000001b3);
                }
            }
        };
        for index in 0..8 {
            let frame = Frame::new(index).unwrap();
            record(zonk_sprite_part(RoundedPhase::Falling(frame)).unwrap());
            record(infotron_sprite_part(RoundedPhase::Falling(frame)).unwrap());
            record(orange_sprite_part(frame));
            for direction in [Horizontal::Left, Horizontal::Right] {
                record(zonk_sprite_part(RoundedPhase::Rolling { direction, frame }).unwrap());
                record(infotron_sprite_part(RoundedPhase::Rolling { direction, frame }).unwrap());
            }
            for turn in [EnemyTurn::Left, EnemyTurn::Right] {
                record(snik_snak_sprite_part(EnemyPhase::Turning { turn, frame }));
                record(electron_sprite_part(EnemyPhase::Turning { turn, frame }));
            }
            for direction in Direction::ALL {
                record(snik_snak_sprite_part(EnemyPhase::Moving {
                    direction,
                    frame,
                }));
                record(electron_sprite_part(EnemyPhase::Moving {
                    direction,
                    frame,
                }));
            }
            record(explosion_sprite_part(ExplosionResidue::Empty, frame));
            record(explosion_sprite_part(ExplosionResidue::Infotron, frame));
        }
        for index in 0..14 {
            record(bug_sprite_part(Frame::new(index).unwrap()));
        }
        // Captured from the pre-refactor selectors, not generated by this implementation.
        assert_eq!(
            digest, 0xc972aa2eabf028cd,
            "an original sprite rectangle changed"
        );
    }

    /// Retains the old camera interpolation at every bounded fall and roll picture.
    #[test]
    fn camera_offsets_preserve_original_inclusive_endpoints() {
        use crate::actors::{Actor, State, Zonk};
        let distances = match super::TILE_SIZE {
            32 => [32, 27, 23, 18, 14, 9, 5, 0],
            16 => [16, 14, 11, 9, 7, 5, 2, 0],
            _ => panic!("the game has only desktop and native-tile scales"),
        };
        for (index, distance) in distances.into_iter().enumerate() {
            let frame = Frame::new(index as u8).unwrap();
            let falling = State::new(Actor::Zonk(Zonk::from_phase(RoundedPhase::Falling(frame))));
            assert_eq!(super::movement_offset(&falling), (0, -distance));
            for (direction, x) in [(Horizontal::Left, distance), (Horizontal::Right, -distance)] {
                let rolling = State::new(Actor::Zonk(Zonk::from_phase(RoundedPhase::Rolling {
                    direction,
                    frame,
                })));
                assert_eq!(super::movement_offset(&rolling), (x, -distance));
            }
        }
    }
}
