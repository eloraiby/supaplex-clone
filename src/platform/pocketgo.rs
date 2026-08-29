//! Direct Linux devices for the original ARMv5TE PocketGo.
//!
//! This module deliberately mirrors only the small rust-sdl2 API surface used
//! by the game. It writes RGB565 frames to `/dev/fb0`, reads the Miyoo kernel
//! keyboard through evdev when available or the active Linux console on older
//! firmware, and streams signed 16-bit stereo samples to OSS `/dev/dsp`.

use std::io;

/// No process-global library is needed by the native PocketGo backend.
pub struct Sdl;

pub fn init() -> Result<Sdl, String> {
    Ok(Sdl)
}

impl Sdl {
    pub fn video(&self) -> Result<video::VideoSubsystem, String> {
        Ok(video::VideoSubsystem)
    }

    pub fn audio(&self) -> Result<AudioSubsystem, String> {
        Ok(AudioSubsystem)
    }

    pub fn event_pump(&self) -> Result<EventPump, String> {
        EventPump::new()
    }
}

pub mod hint {
    pub fn set(_name: &str, _value: &str) -> bool {
        false
    }
}

pub mod pixels {
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub struct Color {
        pub r: u8,
        pub g: u8,
        pub b: u8,
        pub a: u8,
    }

    #[allow(non_snake_case)]
    impl Color {
        pub const fn RGB(r: u8, g: u8, b: u8) -> Self {
            Self { r, g, b, a: 255 }
        }

        pub const fn RGBA(r: u8, g: u8, b: u8, a: u8) -> Self {
            Self { r, g, b, a }
        }
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub enum PixelFormatEnum {
        RGBA32,
    }
}

pub mod rect {
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub struct Rect {
        x: i32,
        y: i32,
        width: u32,
        height: u32,
    }

    impl Rect {
        pub const fn new(x: i32, y: i32, width: u32, height: u32) -> Self {
            Self {
                x,
                y,
                width,
                height,
            }
        }

        pub const fn x(self) -> i32 {
            self.x
        }

        pub const fn y(self) -> i32 {
            self.y
        }

        pub const fn width(self) -> u32 {
            self.width
        }

        pub const fn height(self) -> u32 {
            self.height
        }

        pub fn right(self) -> i32 {
            self.x
                .saturating_add(self.width.min(i32::MAX as u32) as i32)
        }

        pub fn bottom(self) -> i32 {
            self.y
                .saturating_add(self.height.min(i32::MAX as u32) as i32)
        }

        pub fn has_intersection(self, other: Self) -> bool {
            let self_right = i64::from(self.x) + i64::from(self.width);
            let self_bottom = i64::from(self.y) + i64::from(self.height);
            let other_right = i64::from(other.x) + i64::from(other.width);
            let other_bottom = i64::from(other.y) + i64::from(other.height);
            i64::from(self.x) < other_right
                && i64::from(other.x) < self_right
                && i64::from(self.y) < other_bottom
                && i64::from(other.y) < self_bottom
        }
    }
}

pub mod video {
    use std::{
        fs::{File, OpenOptions},
        marker::PhantomData,
        os::fd::AsRawFd,
        ptr::NonNull,
    };

    use crate::platform::{keyboard::TextInputUtil, render::CanvasBuilder};

    pub const WIDTH: u32 = 320;
    pub const HEIGHT: u32 = 240;
    const PIXEL_COUNT: usize = WIDTH as usize * HEIGHT as usize;
    const FRAME_BYTES: usize = PIXEL_COUNT * std::mem::size_of::<u16>();

    #[derive(Clone)]
    pub struct VideoSubsystem;

    impl VideoSubsystem {
        pub fn window<'a>(&'a self, title: &str, width: u32, height: u32) -> WindowBuilder<'a> {
            WindowBuilder {
                title: title.to_owned(),
                width,
                height,
                _video: PhantomData,
            }
        }

        pub fn text_input(&self) -> TextInputUtil {
            TextInputUtil
        }
    }

    pub struct WindowBuilder<'a> {
        title: String,
        width: u32,
        height: u32,
        _video: PhantomData<&'a VideoSubsystem>,
    }

    impl WindowBuilder<'_> {
        pub fn position_centered(self) -> Self {
            self
        }

        pub fn resizable(self) -> Self {
            self
        }

        pub fn build(self) -> Result<Window, String> {
            if self.width != WIDTH || self.height != HEIGHT {
                return Err(format!(
                    "PocketGo framebuffer is {WIDTH}x{HEIGHT}, requested {}x{}",
                    self.width, self.height
                ));
            }
            let framebuffer = Framebuffer::open()?;
            let mut window = Window { framebuffer };
            window.set_title(&self.title)?;
            Ok(window)
        }
    }

    pub struct Window {
        pub(crate) framebuffer: Framebuffer,
    }

    impl Window {
        pub fn into_canvas(self) -> CanvasBuilder {
            CanvasBuilder { window: self }
        }

        pub fn set_title(&mut self, _title: &str) -> Result<(), String> {
            Ok(())
        }
    }

    #[derive(Clone, Copy)]
    pub struct WindowContext;

    pub(crate) struct Framebuffer {
        _file: File,
        memory: NonNull<u16>,
    }

    impl Framebuffer {
        fn open() -> Result<Self, String> {
            validate_mode()?;
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .open("/dev/fb0")
                .map_err(|error| format!("open /dev/fb0: {error}"))?;
            // SAFETY: the Miyoo framebuffer driver exports at least one fixed
            // 320x240x16 frame. The File keeps the mapping's device alive.
            let memory = unsafe {
                libc::mmap(
                    std::ptr::null_mut(),
                    FRAME_BYTES,
                    libc::PROT_READ | libc::PROT_WRITE,
                    libc::MAP_SHARED,
                    file.as_raw_fd(),
                    0,
                )
            };
            if memory == libc::MAP_FAILED {
                return Err(format!("map /dev/fb0: {}", std::io::Error::last_os_error()));
            }
            let memory = NonNull::new(memory.cast::<u16>())
                .ok_or_else(|| "map /dev/fb0 returned null".to_owned())?;
            Ok(Self {
                _file: file,
                memory,
            })
        }

        pub(crate) fn present(&mut self, rgb565: &[u16]) {
            debug_assert_eq!(rgb565.len(), PIXEL_COUNT);
            // SAFETY: both buffers contain exactly one framebuffer-sized frame
            // and do not overlap. The kernel scans this coherent mapped memory.
            unsafe {
                std::ptr::copy_nonoverlapping(rgb565.as_ptr(), self.memory.as_ptr(), PIXEL_COUNT)
            };
        }
    }

    impl Drop for Framebuffer {
        fn drop(&mut self) {
            // SAFETY: this is the mapping created in Framebuffer::open.
            unsafe { libc::munmap(self.memory.as_ptr().cast(), FRAME_BYTES) };
        }
    }

    fn validate_mode() -> Result<(), String> {
        let bits = std::fs::read_to_string("/sys/class/graphics/fb0/bits_per_pixel")
            .map_err(|error| format!("read fb0 pixel format: {error}"))?;
        if bits.trim() != "16" {
            return Err(format!(
                "PocketGo backend requires RGB565 fb0, found {bits:?}"
            ));
        }
        let size = std::fs::read_to_string("/sys/class/graphics/fb0/virtual_size")
            .map_err(|error| format!("read fb0 virtual size: {error}"))?;
        let mut dimensions = size.trim().split(',');
        let width = dimensions
            .next()
            .and_then(|value| value.parse::<u32>().ok());
        let height = dimensions
            .next()
            .and_then(|value| value.parse::<u32>().ok());
        if width != Some(WIDTH) || height.is_none_or(|height| height < HEIGHT) {
            return Err(format!(
                "PocketGo backend requires at least {WIDTH}x{HEIGHT} fb0, found {size:?}"
            ));
        }
        Ok(())
    }
}

pub mod render {
    use std::{marker::PhantomData, time::Instant};

    use crate::platform::{
        pixels::{Color, PixelFormatEnum},
        rect::Rect,
        video::{HEIGHT, WIDTH, Window, WindowContext},
    };

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub enum BlendMode {
        None,
        Blend,
    }

    pub struct CanvasBuilder {
        pub(crate) window: Window,
    }

    impl CanvasBuilder {
        pub fn present_vsync(self) -> Self {
            self
        }

        pub fn build(self) -> Result<Canvas<Window>, String> {
            Ok(Canvas {
                window: self.window,
                pixels: vec![0; WIDTH as usize * HEIGHT as usize],
                draw_color: Color::RGB(0, 0, 0),
                blend_mode: BlendMode::None,
                stats_started: Instant::now(),
                stats_frames: 0,
            })
        }
    }

    pub struct Canvas<T> {
        window: T,
        pixels: Vec<u16>,
        draw_color: Color,
        blend_mode: BlendMode,
        stats_started: Instant,
        stats_frames: u64,
    }

    impl Canvas<Window> {
        pub fn set_logical_size(&mut self, width: u32, height: u32) -> Result<(), String> {
            if width == WIDTH && height == HEIGHT {
                Ok(())
            } else {
                Err(format!("PocketGo logical size must be {WIDTH}x{HEIGHT}"))
            }
        }

        pub fn texture_creator(&self) -> TextureCreator<WindowContext> {
            TextureCreator(PhantomData)
        }

        pub fn window_mut(&mut self) -> &mut Window {
            &mut self.window
        }

        pub fn set_draw_color(&mut self, color: Color) {
            self.draw_color = color;
        }

        pub fn set_blend_mode(&mut self, mode: BlendMode) {
            self.blend_mode = mode;
        }

        pub fn clear(&mut self) {
            self.pixels.fill(rgb565(
                self.draw_color.r,
                self.draw_color.g,
                self.draw_color.b,
            ));
        }

        pub fn fill_rect(&mut self, rect: Rect) -> Result<(), String> {
            if let Some((left, top, right, bottom)) = clip(rect) {
                let source = rgb565(self.draw_color.r, self.draw_color.g, self.draw_color.b);
                if self.blend_mode == BlendMode::None || self.draw_color.a == 255 {
                    for y in top..bottom {
                        let start = y * WIDTH as usize + left;
                        self.pixels[start..start + right - left].fill(source);
                    }
                    return Ok(());
                }
                for y in top..bottom {
                    for x in left..right {
                        let index = y * WIDTH as usize + x;
                        blend_rgb565(
                            &mut self.pixels[index],
                            source,
                            self.draw_color.a,
                            self.blend_mode,
                        );
                    }
                }
            }
            Ok(())
        }

        pub fn draw_rect(&mut self, rect: Rect) -> Result<(), String> {
            if rect.width() == 0 || rect.height() == 0 {
                return Ok(());
            }
            self.fill_rect(Rect::new(rect.x(), rect.y(), rect.width(), 1))?;
            self.fill_rect(Rect::new(
                rect.x(),
                rect.y() + rect.height() as i32 - 1,
                rect.width(),
                1,
            ))?;
            self.fill_rect(Rect::new(rect.x(), rect.y(), 1, rect.height()))?;
            self.fill_rect(Rect::new(
                rect.x() + rect.width() as i32 - 1,
                rect.y(),
                1,
                rect.height(),
            ))
        }

        pub fn copy<S, D>(
            &mut self,
            texture: &Texture<'_>,
            source: S,
            destination: D,
        ) -> Result<(), String>
        where
            S: Into<Option<Rect>>,
            D: Into<Option<Rect>>,
        {
            let source = source
                .into()
                .unwrap_or_else(|| Rect::new(0, 0, texture.width, texture.height));
            let destination = destination
                .into()
                .unwrap_or_else(|| Rect::new(0, 0, WIDTH, HEIGHT));
            if source.width() == 0
                || source.height() == 0
                || destination.width() == 0
                || destination.height() == 0
            {
                return Ok(());
            }
            let Some((left, top, right, bottom)) = clip(destination) else {
                return Ok(());
            };
            if source.width() == destination.width()
                && source.height() == destination.height()
                && self.copy_unscaled(texture, source, destination, left, top, right, bottom)
            {
                return Ok(());
            }
            for y in top..bottom {
                let source_y = i64::from(source.y())
                    + (y as i64 - i64::from(destination.y())) * i64::from(source.height())
                        / i64::from(destination.height());
                for x in left..right {
                    let source_x = i64::from(source.x())
                        + (x as i64 - i64::from(destination.x())) * i64::from(source.width())
                            / i64::from(destination.width());
                    if source_x < 0
                        || source_y < 0
                        || source_x >= i64::from(texture.width)
                        || source_y >= i64::from(texture.height)
                    {
                        continue;
                    }
                    let from = source_y as usize * texture.width as usize + source_x as usize;
                    let to = y * WIDTH as usize + x;
                    let source = modulate_rgb565(texture.pixels[from], texture.color);
                    blend_rgb565(
                        &mut self.pixels[to],
                        source,
                        texture.alpha[from],
                        texture.blend_mode,
                    );
                }
            }
            Ok(())
        }

        /// Copies an unscaled, in-bounds source after destination clipping.
        ///
        /// Every bundled PocketGo texture is rendered at its original pixel
        /// size. Keeping this separate avoids two runtime i64 divisions for
        /// every pixel, which are software helper calls on ARMv5TE.
        #[allow(clippy::too_many_arguments)]
        fn copy_unscaled(
            &mut self,
            texture: &Texture<'_>,
            source: Rect,
            destination: Rect,
            left: usize,
            top: usize,
            right: usize,
            bottom: usize,
        ) -> bool {
            let source_left = i64::from(source.x()) + left as i64 - i64::from(destination.x());
            let source_top = i64::from(source.y()) + top as i64 - i64::from(destination.y());
            let width = right - left;
            let height = bottom - top;
            if source_left < 0
                || source_top < 0
                || source_left + width as i64 > i64::from(texture.width)
                || source_top + height as i64 > i64::from(texture.height)
            {
                return false;
            }
            let source_left = source_left as usize;
            let source_top = source_top as usize;
            let texture_width = texture.width as usize;
            let canvas_width = WIDTH as usize;

            let neutral_color =
                texture.color.r == 255 && texture.color.g == 255 && texture.color.b == 255;
            if texture.blend_mode == BlendMode::None && neutral_color {
                for row in 0..height {
                    let from = (source_top + row) * texture_width + source_left;
                    let to = (top + row) * canvas_width + left;
                    self.pixels[to..to + width]
                        .copy_from_slice(&texture.pixels[from..from + width]);
                }
                return true;
            }

            for row in 0..height {
                let from_row = (source_top + row) * texture_width + source_left;
                let to_row = (top + row) * canvas_width + left;
                for column in 0..width {
                    let from = from_row + column;
                    let to = to_row + column;
                    let source = if neutral_color {
                        texture.pixels[from]
                    } else {
                        modulate_rgb565(texture.pixels[from], texture.color)
                    };
                    blend_rgb565(
                        &mut self.pixels[to],
                        source,
                        texture.alpha[from],
                        texture.blend_mode,
                    );
                }
            }
            true
        }

        pub fn present(&mut self) {
            self.window.framebuffer.present(&self.pixels);
            self.stats_frames += 1;
            let elapsed = self.stats_started.elapsed();
            if elapsed.as_secs() >= 5 {
                let elapsed_millis = elapsed.as_millis().min(u128::from(u64::MAX)) as u64;
                let fps_tenths = self.stats_frames.saturating_mul(10_000) / elapsed_millis.max(1);
                eprintln!("supaplex_video_fps={}.{}", fps_tenths / 10, fps_tenths % 10);
                self.stats_started = Instant::now();
                self.stats_frames = 0;
            }
        }
    }

    fn clip(rect: Rect) -> Option<(usize, usize, usize, usize)> {
        let left = i64::from(rect.x()).clamp(0, i64::from(WIDTH));
        let top = i64::from(rect.y()).clamp(0, i64::from(HEIGHT));
        let right = (i64::from(rect.x()) + i64::from(rect.width())).clamp(0, i64::from(WIDTH));
        let bottom = (i64::from(rect.y()) + i64::from(rect.height())).clamp(0, i64::from(HEIGHT));
        (left < right && top < bottom).then_some((
            left as usize,
            top as usize,
            right as usize,
            bottom as usize,
        ))
    }

    fn rgb565(r: u8, g: u8, b: u8) -> u16 {
        (u16::from(r & 0xf8) << 8) | (u16::from(g & 0xfc) << 3) | (u16::from(b) >> 3)
    }

    fn modulate_rgb565(pixel: u16, color: Color) -> u16 {
        if color.r == 255 && color.g == 255 && color.b == 255 {
            return pixel;
        }
        let red = pixel >> 11;
        let green = (pixel >> 5) & 0x3f;
        let blue = pixel & 0x1f;
        let red = (red << 3) | (red >> 2);
        let green = (green << 2) | (green >> 4);
        let blue = (blue << 3) | (blue >> 2);
        rgb565(
            (red * u16::from(color.r) / 255) as u8,
            (green * u16::from(color.g) / 255) as u8,
            (blue * u16::from(color.b) / 255) as u8,
        )
    }

    fn blend_rgb565(destination: &mut u16, source: u16, alpha: u8, mode: BlendMode) {
        if mode == BlendMode::Blend && alpha == 0 {
            return;
        }
        if mode == BlendMode::None || alpha == 255 {
            *destination = source;
            return;
        }
        let alpha = u32::from(alpha);
        let inverse = 255 - alpha;
        let red = (((u32::from(source >> 11) * alpha + u32::from(*destination >> 11) * inverse)
            / 255) as u16)
            << 11;
        let green = (((u32::from((source >> 5) & 0x3f) * alpha
            + u32::from((*destination >> 5) & 0x3f) * inverse)
            / 255) as u16)
            << 5;
        let blue = ((u32::from(source & 0x1f) * alpha + u32::from(*destination & 0x1f) * inverse)
            / 255) as u16;
        *destination = red | green | blue;
    }

    pub struct TextureCreator<T>(PhantomData<T>);

    impl TextureCreator<WindowContext> {
        pub fn create_texture_streaming(
            &self,
            _format: PixelFormatEnum,
            width: u32,
            height: u32,
        ) -> Result<Texture<'_>, String> {
            let length = (width as usize)
                .checked_mul(height as usize)
                .ok_or_else(|| "texture dimensions overflow".to_owned())?;
            Ok(Texture {
                width,
                height,
                pixels: vec![0; length],
                alpha: vec![0; length],
                color: Color::RGB(255, 255, 255),
                blend_mode: BlendMode::None,
                _creator: PhantomData,
            })
        }
    }

    pub struct Texture<'a> {
        width: u32,
        height: u32,
        pixels: Vec<u16>,
        alpha: Vec<u8>,
        color: Color,
        blend_mode: BlendMode,
        _creator: PhantomData<&'a ()>,
    }

    impl Texture<'_> {
        pub fn update(
            &mut self,
            rect: Option<Rect>,
            pixels: &[u8],
            pitch: usize,
        ) -> Result<(), String> {
            let rect = rect.unwrap_or_else(|| Rect::new(0, 0, self.width, self.height));
            if rect.x() < 0
                || rect.y() < 0
                || rect.x() as u32 + rect.width() > self.width
                || rect.y() as u32 + rect.height() > self.height
            {
                return Err("texture update is out of bounds".to_owned());
            }
            let row_bytes = rect.width() as usize * 4;
            let required = pitch
                .checked_mul(rect.height().saturating_sub(1) as usize)
                .and_then(|length| length.checked_add(row_bytes))
                .ok_or_else(|| "texture update length overflow".to_owned())?;
            if pitch < row_bytes || pixels.len() < required {
                return Err("texture update buffer is too short".to_owned());
            }
            for row in 0..rect.height() as usize {
                let source = &pixels[row * pitch..row * pitch + row_bytes];
                let start = (rect.y() as usize + row) * self.width as usize + rect.x() as usize;
                for column in 0..rect.width() as usize {
                    let from = column * 4;
                    self.pixels[start + column] =
                        rgb565(source[from], source[from + 1], source[from + 2]);
                    self.alpha[start + column] = source[from + 3];
                }
            }
            Ok(())
        }

        pub fn set_blend_mode(&mut self, mode: BlendMode) {
            self.blend_mode = mode;
        }

        pub fn set_color_mod(&mut self, r: u8, g: u8, b: u8) {
            self.color = Color::RGB(r, g, b);
        }
    }

    #[cfg(test)]
    mod tests {
        use super::{BlendMode, blend_rgb565, modulate_rgb565, rgb565};
        use crate::platform::pixels::Color;

        #[test]
        fn native_pixels_pack_rgb_channels_into_framebuffer_order() {
            assert_eq!(rgb565(255, 0, 0), 0xf800);
            assert_eq!(rgb565(0, 255, 0), 0x07e0);
            assert_eq!(rgb565(0, 0, 255), 0x001f);
            assert_eq!(rgb565(255, 255, 255), 0xffff);
        }

        #[test]
        fn white_font_pixels_modulate_directly_to_the_requested_color() {
            let color = Color::RGB(0xe0, 0x10, 0x70);
            assert_eq!(modulate_rgb565(0xffff, color), rgb565(0xe0, 0x10, 0x70));
        }

        #[test]
        fn rgb565_blending_handles_transparent_opaque_and_half_black() {
            let mut destination = 0xffff;
            blend_rgb565(&mut destination, 0, 0, BlendMode::Blend);
            assert_eq!(destination, 0xffff);
            blend_rgb565(&mut destination, 0xf800, 255, BlendMode::Blend);
            assert_eq!(destination, 0xf800);
            destination = 0xffff;
            blend_rgb565(&mut destination, 0, 128, BlendMode::Blend);
            assert_eq!(destination, 0x7bef);
        }
    }
}

pub mod mouse {
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub enum MouseButton {
        Left,
        Right,
    }
}

pub mod keyboard {
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub enum Scancode {
        Backspace,
        C,
        D,
        Delete,
        Down,
        End,
        Escape,
        F1,
        F2,
        F3,
        F4,
        F5,
        F6,
        F7,
        F8,
        F9,
        F10,
        G,
        Home,
        K,
        KpEnter,
        LAlt,
        LCtrl,
        Left,
        LShift,
        M,
        N,
        PageDown,
        PageUp,
        R,
        Return,
        Right,
        S,
        Space,
        T,
        Tab,
        Up,
    }

    pub(crate) const fn key_code(scancode: Scancode) -> u16 {
        match scancode {
            Scancode::Escape => 1,
            Scancode::Backspace => 14,
            Scancode::Tab => 15,
            Scancode::R => 19,
            Scancode::T => 20,
            Scancode::Return => 28,
            Scancode::LCtrl => 29,
            Scancode::S => 31,
            Scancode::D => 32,
            Scancode::G => 34,
            Scancode::K => 37,
            Scancode::LShift => 42,
            Scancode::C => 46,
            Scancode::N => 49,
            Scancode::M => 50,
            Scancode::LAlt => 56,
            Scancode::Space => 57,
            Scancode::F1 => 59,
            Scancode::F2 => 60,
            Scancode::F3 => 61,
            Scancode::F4 => 62,
            Scancode::F5 => 63,
            Scancode::F6 => 64,
            Scancode::F7 => 65,
            Scancode::F8 => 66,
            Scancode::F9 => 67,
            Scancode::F10 => 68,
            Scancode::KpEnter => 96,
            Scancode::Home => 102,
            Scancode::Up => 103,
            Scancode::PageUp => 104,
            Scancode::Left => 105,
            Scancode::Right => 106,
            Scancode::End => 107,
            Scancode::Down => 108,
            Scancode::PageDown => 109,
            Scancode::Delete => 111,
        }
    }

    pub(crate) fn from_key_code(code: u16) -> Option<Scancode> {
        const KEYS: &[Scancode] = &[
            Scancode::Backspace,
            Scancode::C,
            Scancode::D,
            Scancode::Delete,
            Scancode::Down,
            Scancode::End,
            Scancode::Escape,
            Scancode::F1,
            Scancode::F2,
            Scancode::F3,
            Scancode::F4,
            Scancode::F5,
            Scancode::F6,
            Scancode::F7,
            Scancode::F8,
            Scancode::F9,
            Scancode::F10,
            Scancode::G,
            Scancode::Home,
            Scancode::K,
            Scancode::KpEnter,
            Scancode::LAlt,
            Scancode::LCtrl,
            Scancode::Left,
            Scancode::LShift,
            Scancode::M,
            Scancode::N,
            Scancode::PageDown,
            Scancode::PageUp,
            Scancode::R,
            Scancode::Return,
            Scancode::Right,
            Scancode::S,
            Scancode::Space,
            Scancode::T,
            Scancode::Tab,
            Scancode::Up,
        ];
        KEYS.iter().copied().find(|key| key_code(*key) == code)
    }

    pub struct KeyboardState<'a> {
        pub(crate) pressed: &'a [bool],
    }

    impl KeyboardState<'_> {
        pub fn is_scancode_pressed(&self, scancode: Scancode) -> bool {
            self.pressed
                .get(key_code(scancode) as usize)
                .copied()
                .unwrap_or(false)
        }
    }

    #[derive(Clone, Copy)]
    pub struct TextInputUtil;

    impl TextInputUtil {
        pub fn start(&self) {}
        pub fn stop(&self) {}
    }
}

pub mod event {
    use crate::platform::{keyboard::Scancode, mouse::MouseButton};

    #[derive(Clone, Debug, Eq, PartialEq)]
    pub enum Event {
        Quit {
            timestamp: u32,
        },
        KeyDown {
            timestamp: u32,
            window_id: u32,
            scancode: Option<Scancode>,
            repeat: bool,
        },
        MouseMotion {
            timestamp: u32,
            window_id: u32,
            x: i32,
            y: i32,
        },
        MouseButtonDown {
            timestamp: u32,
            window_id: u32,
            mouse_btn: MouseButton,
            x: i32,
            y: i32,
        },
        TextInput {
            timestamp: u32,
            window_id: u32,
            text: String,
        },
    }
}

use std::{
    fs::{File, OpenOptions},
    io::Read,
    mem,
    os::{fd::AsRawFd, unix::fs::OpenOptionsExt},
    path::{Path, PathBuf},
};

use event::Event;

const EV_KEY: u16 = 1;
const KEY_STATE_COUNT: usize = 512;
const KDGKBMODE: libc::c_ulong = 0x4b44;
const KDSKBMODE: libc::c_ulong = 0x4b45;
const K_MEDIUMRAW: libc::c_int = 2;

#[repr(C)]
#[derive(Clone, Copy)]
struct InputEvent {
    time: libc::timeval,
    event_type: u16,
    code: u16,
    value: i32,
}

pub struct EventPump {
    input: InputSource,
    pressed: [bool; KEY_STATE_COUNT],
}

impl EventPump {
    fn new() -> Result<Self, String> {
        Ok(Self {
            input: open_keypad()?,
            pressed: [false; KEY_STATE_COUNT],
        })
    }

    pub fn poll_iter(&mut self) -> EventPollIterator<'_> {
        EventPollIterator { pump: self }
    }

    pub fn keyboard_state(&self) -> keyboard::KeyboardState<'_> {
        keyboard::KeyboardState {
            pressed: &self.pressed,
        }
    }
}

pub struct EventPollIterator<'a> {
    pump: &'a mut EventPump,
}

impl Iterator for EventPollIterator<'_> {
    type Item = Event;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            match self.pump.input.read_change() {
                Ok(Some(change)) if change.code as usize >= KEY_STATE_COUNT => continue,
                Ok(Some(change)) => {
                    let was_pressed = self.pump.pressed[change.code as usize];
                    self.pump.pressed[change.code as usize] = change.pressed;
                    if !change.pressed {
                        continue;
                    }
                    return Some(Event::KeyDown {
                        timestamp: 0,
                        window_id: 0,
                        scancode: keyboard::from_key_code(change.code),
                        repeat: change.repeat || was_pressed,
                    });
                }
                Ok(None) => return None,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => return None,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => return None,
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct KeyChange {
    code: u16,
    pressed: bool,
    repeat: bool,
}

enum InputSource {
    Evdev(File),
    Console(ConsoleKeyboard),
}

impl InputSource {
    fn read_change(&mut self) -> io::Result<Option<KeyChange>> {
        match self {
            Self::Evdev(file) => read_evdev_change(file),
            Self::Console(console) => console.read_change(),
        }
    }
}

fn read_evdev_change(file: &mut File) -> io::Result<Option<KeyChange>> {
    loop {
        let mut bytes = [0_u8; mem::size_of::<InputEvent>()];
        let length = file.read(&mut bytes)?;
        if length == 0 {
            return Ok(None);
        }
        if length != bytes.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("short evdev input_event: {length}/{} bytes", bytes.len()),
            ));
        }
        // SAFETY: bytes contains one complete native input_event.
        let input = unsafe { std::ptr::read_unaligned(bytes.as_ptr().cast::<InputEvent>()) };
        if input.event_type == EV_KEY {
            return Ok(Some(KeyChange {
                code: input.code,
                pressed: input.value != 0,
                repeat: input.value == 2,
            }));
        }
    }
}

fn open_keypad() -> Result<InputSource, String> {
    if let Some(path) = std::env::var_os("SUPAPLEX_INPUT_DEVICE") {
        return open_input(PathBuf::from(path)).map(InputSource::Evdev);
    }
    let mut fallback = None;
    for index in 0..16 {
        let path = PathBuf::from(format!("/dev/input/event{index}"));
        let Ok(file) = open_input(path) else {
            continue;
        };
        if input_name(&file).as_deref() == Some("miyoo_keypad") {
            eprintln!("supaplex_input=evdev device=/dev/input/event{index}");
            return Ok(InputSource::Evdev(file));
        }
        fallback.get_or_insert(file);
    }
    if let Some(file) = fallback {
        eprintln!("supaplex_input=evdev device=unnamed_fallback");
        return Ok(InputSource::Evdev(file));
    }

    open_console_keyboard()
        .map(InputSource::Console)
        .map_err(|error| {
            format!("could not open /dev/input/event* or a Linux console keyboard: {error}")
        })
}

fn open_input(path: PathBuf) -> Result<File, String> {
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(&path)
        .map_err(|error| format!("open {}: {error}", path.display()))
}

fn input_name(file: &File) -> Option<String> {
    let mut name = [0_u8; 64];
    // EVIOCGNAME is _IOR('E', 0x06, len). Linux's generic ioctl encoding is
    // shared by ARM EABI and the desktop host used for compile-time checks.
    let request = (2_u64 << 30) | ((name.len() as u64) << 16) | (u64::from(b'E') << 8) | 0x06;
    // SAFETY: the request writes at most name.len() bytes into this buffer.
    if unsafe {
        libc::ioctl(
            file.as_raw_fd(),
            request as libc::c_ulong,
            name.as_mut_ptr(),
        )
    } < 0
    {
        return None;
    }
    let length = name
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(name.len());
    Some(String::from_utf8_lossy(&name[..length]).into_owned())
}

struct ConsoleKeyboard {
    file: File,
    saved_mode: libc::c_int,
    saved_termios: libc::termios,
}

impl ConsoleKeyboard {
    fn open(path: &Path) -> Result<Self, String> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(path)
            .map_err(|error| format!("open {}: {error}", path.display()))?;
        let fd = file.as_raw_fd();
        // SAFETY: termios is a plain C structure initialized by tcgetattr.
        let mut saved_termios = unsafe { mem::zeroed::<libc::termios>() };
        // SAFETY: fd is an open console candidate and saved_termios is writable.
        if unsafe { libc::tcgetattr(fd, &mut saved_termios) } < 0 {
            return Err(format!(
                "tcgetattr {}: {}",
                path.display(),
                io::Error::last_os_error()
            ));
        }
        let mut saved_mode = 0_i32;
        // SAFETY: KDGKBMODE writes one C int to the supplied pointer.
        if unsafe { libc::ioctl(fd, KDGKBMODE, &mut saved_mode) } < 0 {
            return Err(format!(
                "KDGKBMODE {}: {}",
                path.display(),
                io::Error::last_os_error()
            ));
        }

        let mut raw = saved_termios;
        raw.c_iflag = 0;
        raw.c_oflag = 0;
        raw.c_lflag &= !(libc::ICANON | libc::ECHO | libc::ISIG);
        raw.c_cc[libc::VMIN] = 0;
        raw.c_cc[libc::VTIME] = 0;
        // SAFETY: raw is a valid termios copied from this terminal.
        if unsafe { libc::tcsetattr(fd, libc::TCSAFLUSH, &raw) } < 0 {
            return Err(format!(
                "tcsetattr {}: {}",
                path.display(),
                io::Error::last_os_error()
            ));
        }
        // SAFETY: KDSKBMODE takes the keyboard mode as its integer argument.
        if unsafe { libc::ioctl(fd, KDSKBMODE, K_MEDIUMRAW) } < 0 {
            // SAFETY: best-effort restoration after the partially completed setup.
            unsafe { libc::tcsetattr(fd, libc::TCSAFLUSH, &saved_termios) };
            return Err(format!(
                "KDSKBMODE {}: {}",
                path.display(),
                io::Error::last_os_error()
            ));
        }

        eprintln!(
            "supaplex_input=linux_console_mediumraw device={}",
            path.display()
        );
        Ok(Self {
            file,
            saved_mode,
            saved_termios,
        })
    }

    fn read_change(&mut self) -> io::Result<Option<KeyChange>> {
        let mut byte = [0_u8; 1];
        if self.file.read(&mut byte)? == 0 {
            return Ok(None);
        }
        Ok(medium_raw_change(byte[0]))
    }
}

impl Drop for ConsoleKeyboard {
    fn drop(&mut self) {
        let fd = self.file.as_raw_fd();
        // SAFETY: restore the mode and termios captured from this same console.
        unsafe {
            libc::ioctl(fd, KDSKBMODE, self.saved_mode);
            libc::tcsetattr(fd, libc::TCSAFLUSH, &self.saved_termios);
        }
    }
}

fn medium_raw_change(byte: u8) -> Option<KeyChange> {
    let code = u16::from(byte & 0x7f);
    // Extended Linux keycodes use an escape sequence beginning with zero. The
    // PocketGo driver reports only codes 1..=109, so zero can be ignored.
    (code != 0).then_some(KeyChange {
        code,
        pressed: byte & 0x80 == 0,
        repeat: false,
    })
}

fn open_console_keyboard() -> Result<ConsoleKeyboard, String> {
    if let Some(path) = std::env::var_os("SUPAPLEX_TTY_DEVICE") {
        return ConsoleKeyboard::open(Path::new(&path));
    }

    let mut errors = Vec::new();
    for path in ["/dev/tty", "/dev/tty0", "/dev/console"] {
        match ConsoleKeyboard::open(Path::new(path)) {
            Ok(console) => return Ok(console),
            Err(error) => errors.push(error),
        }
    }
    Err(errors.join("; "))
}

#[cfg(test)]
mod input_tests {
    use super::{KeyChange, medium_raw_change};

    #[test]
    fn medium_raw_bytes_preserve_linux_keycodes_and_release_state() {
        assert_eq!(
            medium_raw_change(103),
            Some(KeyChange {
                code: 103,
                pressed: true,
                repeat: false,
            })
        );
        assert_eq!(
            medium_raw_change(103 | 0x80),
            Some(KeyChange {
                code: 103,
                pressed: false,
                repeat: false,
            })
        );
    }

    #[test]
    fn medium_raw_extended_prefix_is_ignored_for_pocketgo_keys() {
        assert_eq!(medium_raw_change(0), None);
    }
}

pub mod audio {
    use std::{
        fs::OpenOptions,
        io::Write,
        marker::PhantomData,
        os::fd::AsRawFd,
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
        thread::{self, JoinHandle},
        time::Duration,
    };

    const AFMT_S16_LE: i32 = 0x10;
    const SNDCTL_DSP_SPEED: libc::c_ulong = 0xc004_5002;
    const SNDCTL_DSP_SETFMT: libc::c_ulong = 0xc004_5005;
    const SNDCTL_DSP_CHANNELS: libc::c_ulong = 0xc004_5006;
    const SNDCTL_DSP_SETFRAGMENT: libc::c_ulong = 0xc004_500a;

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub struct AudioFormat(i32);

    impl AudioFormat {
        pub const fn s16_sys() -> Self {
            Self(AFMT_S16_LE)
        }

        /// The game-facing callback is floating point; the native audio thread
        /// converts it to the signed 16-bit OSS device format after mixing.
        pub const fn f32_sys() -> Self {
            Self(AFMT_S16_LE)
        }
    }

    #[derive(Clone, Copy, Debug)]
    pub struct AudioSpec {
        pub freq: i32,
        pub format: AudioFormat,
        pub channels: u8,
        pub silence: u8,
        pub samples: u16,
        pub size: u32,
    }

    #[derive(Clone, Copy, Debug)]
    pub struct AudioSpecDesired {
        pub freq: Option<i32>,
        pub channels: Option<u8>,
        pub samples: Option<u16>,
    }

    pub trait AudioCallback: Send + 'static {
        type Channel;
        fn callback(&mut self, output: &mut [Self::Channel]);
    }

    pub struct AudioDevice<C> {
        stopped: Arc<AtomicBool>,
        paused: Arc<AtomicBool>,
        thread: Option<JoinHandle<()>>,
        _callback: PhantomData<C>,
    }

    impl<C> AudioDevice<C> {
        pub fn resume(&self) {
            self.paused.store(false, Ordering::Release);
        }
    }

    impl<C> Drop for AudioDevice<C> {
        fn drop(&mut self) {
            self.stopped.store(true, Ordering::Release);
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }

    pub(crate) fn open<C, F>(
        desired: &AudioSpecDesired,
        constructor: F,
    ) -> Result<AudioDevice<C>, String>
    where
        C: AudioCallback<Channel = f32>,
        F: FnOnce(AudioSpec) -> C,
    {
        let mut device = OpenOptions::new()
            .write(true)
            .open("/dev/dsp")
            .map_err(|error| format!("open /dev/dsp: {error}"))?;
        let fd = device.as_raw_fd();
        let mut fragments = (4_i32 << 16) | 11;
        let _ = set(fd, SNDCTL_DSP_SETFRAGMENT, &mut fragments);
        let mut format = AFMT_S16_LE;
        set(fd, SNDCTL_DSP_SETFMT, &mut format)?;
        let mut channels = i32::from(desired.channels.unwrap_or(2));
        set(fd, SNDCTL_DSP_CHANNELS, &mut channels)?;
        let mut frequency = desired.freq.unwrap_or(44_100);
        set(fd, SNDCTL_DSP_SPEED, &mut frequency)?;
        if format != AFMT_S16_LE || channels != 2 || frequency != desired.freq.unwrap_or(44_100) {
            return Err(format!(
                "OSS returned unsupported format={format:#x}, channels={channels}, rate={frequency}"
            ));
        }
        let samples = desired.samples.unwrap_or(512);
        let spec = AudioSpec {
            freq: frequency,
            format: AudioFormat(format),
            channels: channels as u8,
            silence: 0,
            samples,
            size: u32::from(samples) * channels as u32 * 2,
        };
        let stopped = Arc::new(AtomicBool::new(false));
        let paused = Arc::new(AtomicBool::new(true));
        let thread_stopped = Arc::clone(&stopped);
        let thread_paused = Arc::clone(&paused);
        let mut callback = constructor(spec);
        let thread = thread::Builder::new()
            .name("pocketgo-audio".to_owned())
            .spawn(move || {
                let mut samples = vec![0.0_f32; spec.samples as usize * spec.channels as usize];
                let mut bytes = vec![0_u8; samples.len() * 2];
                while !thread_stopped.load(Ordering::Acquire) {
                    if thread_paused.load(Ordering::Acquire) {
                        thread::sleep(Duration::from_millis(2));
                        continue;
                    }
                    callback.callback(&mut samples);
                    let (sample_bytes, remainder) = bytes.as_chunks_mut::<2>();
                    debug_assert!(remainder.is_empty());
                    for (sample, bytes) in samples.iter().zip(sample_bytes) {
                        let sample = (sample.clamp(-1.0, 1.0) * 32_767.0).round() as i16;
                        bytes.copy_from_slice(&sample.to_le_bytes());
                    }
                    if device.write_all(&bytes).is_err() {
                        break;
                    }
                }
            })
            .map_err(|error| format!("start PocketGo audio thread: {error}"))?;
        Ok(AudioDevice {
            stopped,
            paused,
            thread: Some(thread),
            _callback: PhantomData,
        })
    }

    fn set(fd: i32, request: libc::c_ulong, value: &mut i32) -> Result<(), String> {
        // SAFETY: each OSS request takes one writable C int.
        if unsafe { libc::ioctl(fd, request, value as *mut i32) } < 0 {
            Err(std::io::Error::last_os_error().to_string())
        } else {
            Ok(())
        }
    }
}

#[derive(Clone)]
pub struct AudioSubsystem;

impl AudioSubsystem {
    pub fn open_playback<C, F>(
        &self,
        _device: Option<&str>,
        desired: &audio::AudioSpecDesired,
        constructor: F,
    ) -> Result<audio::AudioDevice<C>, String>
    where
        C: audio::AudioCallback<Channel = f32>,
        F: FnOnce(audio::AudioSpec) -> C,
    {
        audio::open(desired, constructor)
    }
}

pub mod filesystem {
    use std::env;

    pub fn pref_path(_organization: &str, _application: &str) -> Result<String, String> {
        let executable = env::current_exe()
            .map_err(|error| format!("resolve PocketGo executable path: {error}"))?;
        let directory = executable
            .parent()
            .ok_or_else(|| "PocketGo executable path has no parent directory".to_owned())?;
        directory
            .to_str()
            .map(str::to_owned)
            .ok_or_else(|| "PocketGo executable directory is not valid UTF-8".to_owned())
    }

    #[cfg(test)]
    mod tests {
        use std::{env, path::Path};

        #[test]
        fn preference_path_is_the_executable_directory() {
            let executable = env::current_exe().expect("test executable path should resolve");
            let expected = executable
                .parent()
                .expect("test executable should have a parent directory");
            let actual = super::pref_path("eloraiby", "supaplex-clone")
                .expect("PocketGo preference path should resolve");
            assert_eq!(Path::new(&actual), expected);
        }
    }
}
