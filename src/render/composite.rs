//! Composes an actor's DOS bitmap operations before placing it in the scene.
//!
//! Erase pixels belong to an actor's local reconstruction, not to other actors.
//! All local copies are opaque; only the completed frame's exterior background
//! becomes transparent. Enclosed black artwork therefore remains solid.

use super::{
    Camera, DecodedPng, MOVING_SCALE, RenderError, TILE_SIZE, decode_sized_png, upload_texture,
};
use crate::platform::{
    rect::Rect,
    render::{BlendMode, Canvas, Texture, TextureCreator},
    video::{Window, WindowContext},
};
use crate::{
    actors::Position,
    assets::{FIXED_GRAPHICS_PATH, MOVING_GRAPHICS_PATH},
    murphy_animation::SpritePart,
};
use std::collections::HashMap;

/// One opaque bitmap operation within a single actor's local frame.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(super) enum Layer {
    /// Fixed artwork, including material being consumed by Murphy.
    Fixed(SpritePart),
    /// An original animation rectangle, including its local erase pixels.
    Moving(SpritePart),
}

impl Layer {
    /// Returns source geometry without exposing the atlas selection to callers.
    fn part(self) -> SpritePart {
        match self {
            Self::Fixed(part) | Self::Moving(part) => part,
        }
    }
}

/// Finished actor artwork and its displacement from the logical board cell.
pub(super) struct ActorFrame {
    /// Pixels after opaque local reconstruction and exterior-background removal.
    pub(super) image: DecodedPng,
    /// Original-resolution horizontal displacement of the composed image.
    pub(super) offset_x: i32,
    /// Original-resolution vertical displacement of the composed image.
    pub(super) offset_y: i32,
}

/// Uploaded actor artwork reused at any board position or camera offset.
struct CachedFrame<'textures> {
    /// Completed frame with ordinary source-alpha blending.
    texture: Texture<'textures>,
    /// Original-resolution bounding box relative to the actor's logical cell.
    bounds: Rect,
}

/// Owns decoded atlases and caches compositions rather than actor identities.
pub(super) struct FrameCache<'textures> {
    /// Creates textures whose lifetime belongs to the renderer's display context.
    creator: &'textures TextureCreator<WindowContext>,
    /// Opaque fixed artwork used inside local actor composites.
    fixed: DecodedPng,
    /// Opaque moving artwork used inside local actor composites.
    moving: DecodedPng,
    /// A finite set of artwork recipes; positions and camera offsets are excluded.
    frames: HashMap<Vec<Layer>, CachedFrame<'textures>>,
}

impl<'textures> FrameCache<'textures> {
    /// Validates both atlases before a descriptor can address their pixels.
    pub(super) fn new(
        creator: &'textures TextureCreator<WindowContext>,
        fixed: &[u8],
        moving: &[u8],
    ) -> Result<Self, RenderError> {
        Ok(Self {
            creator,
            fixed: decode_sized_png(fixed, 640, 16, FIXED_GRAPHICS_PATH)?,
            moving: decode_sized_png(moving, 320, 462, MOVING_GRAPHICS_PATH)?,
            frames: HashMap::new(),
        })
    }

    /// Draws a completed local frame; no erase operation touches the shared scene.
    pub(super) fn draw(
        &mut self,
        canvas: &mut Canvas<Window>,
        position: Position,
        layers: &[Layer],
        camera: Camera,
    ) -> Result<(), RenderError> {
        if !self.frames.contains_key(layers) {
            // Pixel composition and texture upload happen once for each recipe,
            // not once per actor, board cell, animation loop, or display refresh.
            let frame = compose(layers, &self.fixed, &self.moving);
            let texture = upload_texture(self.creator, &frame.image, BlendMode::Blend)?;
            self.frames.insert(
                layers.to_vec(),
                CachedFrame {
                    texture,
                    bounds: Rect::new(
                        frame.offset_x,
                        frame.offset_y,
                        frame.image.width,
                        frame.image.height,
                    ),
                },
            );
        }
        let cached = &self.frames[layers];
        let bounds = cached.bounds;
        let destination = Rect::new(
            position.x as i32 * TILE_SIZE as i32 - camera.x + bounds.x() * MOVING_SCALE as i32,
            position.y as i32 * TILE_SIZE as i32 - camera.y + bounds.y() * MOVING_SCALE as i32,
            bounds.width() * MOVING_SCALE,
            bounds.height() * MOVING_SCALE,
        );
        canvas
            .copy(&cached.texture, None, destination)
            .map_err(RenderError::Sdl)
    }
}

/// Replays one actor's opaque operations in order onto a private pixel surface.
pub(super) fn compose(layers: &[Layer], fixed: &DecodedPng, moving: &DecodedPng) -> ActorFrame {
    assert!(!layers.is_empty(), "an actor frame needs artwork");
    let left = layers
        .iter()
        .map(|layer| layer.part().offset_x)
        .min()
        .unwrap();
    let top = layers
        .iter()
        .map(|layer| layer.part().offset_y)
        .min()
        .unwrap();
    let right = layers
        .iter()
        .map(|layer| {
            let p = layer.part();
            p.offset_x + p.width as i32
        })
        .max()
        .unwrap();
    let bottom = layers
        .iter()
        .map(|layer| {
            let p = layer.part();
            p.offset_y + p.height as i32
        })
        .max()
        .unwrap();
    let width = (right - left) as u32;
    let height = (bottom - top) as u32;
    let mut image = DecodedPng {
        width,
        height,
        pixels: vec![0; width as usize * height as usize * 4],
    };
    for layer in layers {
        let part = layer.part();
        let atlas = match layer {
            Layer::Fixed(_) => fixed,
            Layer::Moving(_) => moving,
        };
        // Preserve black erase pixels here: they must erase the actor's own
        // previous material layer before any transparency is calculated.
        for y in 0..part.height as usize {
            let source =
                ((part.source.y as usize + y) * atlas.width as usize + part.source.x as usize) * 4;
            let destination = (((part.offset_y - top) as usize + y) * width as usize
                + (part.offset_x - left) as usize)
                * 4;
            let bytes = part.width as usize * 4;
            image.pixels[destination..destination + bytes]
                .copy_from_slice(&atlas.pixels[source..source + bytes]);
            for pixel in image.pixels[destination..destination + bytes]
                .as_chunks_mut::<4>()
                .0
            {
                pixel[3] = 255;
            }
        }
    }
    remove_exterior_background(&mut image);
    ActorFrame {
        image,
        offset_x: left,
        offset_y: top,
    }
}

/// Makes only frame-edge-connected black transparent after local reconstruction.
fn remove_exterior_background(image: &mut DecodedPng) {
    let width = image.width as usize;
    let height = image.height as usize;
    let mut visited = vec![false; width * height];
    let mut pending = Vec::new();
    // Every edge seeds a flood: an actor touching an edge can separate exterior
    // background into several regions. Never cross a colored pixel or a frame.
    for y in 0..height {
        pending.push((0, y));
        pending.push((width - 1, y));
    }
    for x in 0..width {
        pending.push((x, 0));
        pending.push((x, height - 1));
    }
    while let Some((x, y)) = pending.pop() {
        let index = y * width + x;
        if visited[index] {
            continue;
        }
        visited[index] = true;
        let pixel = &mut image.pixels[index * 4..index * 4 + 4];
        match pixel[..3] {
            [0, 0, 0] => pixel[3] = 0,
            _ => continue,
        }
        // Four-connected traversal preserves enclosed black details as solid
        // artwork instead of making holes wherever the DOS palette used black.
        if x > 0 {
            pending.push((x - 1, y));
        }
        if x + 1 < width {
            pending.push((x + 1, y));
        }
        if y > 0 {
            pending.push((x, y - 1));
        }
        if y + 1 < height {
            pending.push((x, y + 1));
        }
    }
}

#[cfg(test)]
mod tests {
    //! Pixel tests for the local reconstruction boundary, independent of SDL.
    use super::*;
    use crate::murphy_animation::SourcePoint;

    /// Builds a descriptor over an entire small test atlas.
    fn entire(image: &DecodedPng) -> SpritePart {
        SpritePart {
            source: SourcePoint { x: 0, y: 0 },
            width: image.width,
            height: image.height,
            offset_x: 0,
            offset_y: 0,
        }
    }

    /// Black erases material inside a local frame without becoming a scene erase.
    #[test]
    fn local_erase_removes_backdrop_before_exterior_becomes_transparent() {
        let fixed = DecodedPng {
            width: 3,
            height: 3,
            pixels: [80, 120, 40, 255].repeat(9),
        };
        let mut moving = DecodedPng {
            width: 3,
            height: 3,
            pixels: [0, 0, 0, 255].repeat(9),
        };
        moving.pixels[16..20].copy_from_slice(&[255, 40, 20, 255]);
        let result = compose(
            &[Layer::Fixed(entire(&fixed)), Layer::Moving(entire(&moving))],
            &fixed,
            &moving,
        );
        for (index, &pixel) in result.image.pixels.as_chunks::<4>().0.iter().enumerate() {
            if index == 4 {
                assert_eq!(pixel, [255, 40, 20, 255]);
            } else {
                assert_eq!(
                    pixel,
                    [0, 0, 0, 0],
                    "consumed backdrop must not remain behind transparent artwork"
                );
            }
        }
    }

    /// The completed layer retains opaque black enclosed by the actor's artwork.
    #[test]
    fn composed_frame_preserves_enclosed_black_details() {
        let mut image = DecodedPng {
            width: 5,
            height: 5,
            pixels: [0, 0, 0, 255].repeat(25),
        };
        for y in 1..4 {
            for x in 1..4 {
                if (x, y) != (2, 2) {
                    image.pixels[(y * 5 + x) * 4..(y * 5 + x) * 4 + 3].fill(127);
                }
            }
        }
        let result = compose(&[Layer::Moving(entire(&image))], &image, &image);
        for y in 0..5 {
            for x in 0..5 {
                let expected = if x == 0 || y == 0 || x == 4 || y == 4 {
                    0
                } else {
                    255
                };
                assert_eq!(result.image.pixels[(y * 5 + x) * 4 + 3], expected);
            }
        }
        assert_eq!(&result.image.pixels[48..52], &[0, 0, 0, 255]);
    }
}
