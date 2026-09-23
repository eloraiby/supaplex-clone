//! Original Murphy animation descriptors and `MOVING.DAT` source coordinates.
//!
//! The DOS engine did not treat Murphy as a grid of interchangeable 16×16
//! frames. Each action selected one of fifty descriptors. A descriptor combines
//! a variably sized source rectangle, a per-update destination offset, and one
//! of the coordinate sequences below. Keeping those tables intact preserves
//! vertical left/right poses, target-specific eating, wide push composites,
//! paired port traversal, and the long Exit disappearance.

use crate::actors::{
    Direction, MurphyAnimation, MurphyMoveTarget, MurphyPushTarget, MurphySnapTarget,
};

/// One pixel coordinate in the unscaled 320×462 `MOVING.DAT` conversion.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct SourcePoint {
    /// Horizontal source pixel measured from the left edge of `moving.png`.
    pub(crate) x: i32,
    /// Vertical source pixel measured from the top edge of `moving.png`.
    pub(crate) y: i32,
}

/// One original descriptor after decoding its little-endian word fields.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Descriptor {
    /// Packed destination offset used by the original 122-byte level pitch.
    offset: i16,
    /// Per-frame offset increment, or the opposite port endpoint offset.
    offset_increment: i16,
    /// Source width expressed as a count of eight-pixel units.
    width_in_eights: u16,
    /// Source height in unscaled pixels.
    height: u16,
    /// Coordinate-sequence index in the original animation pointer table.
    animation_index: u8,
}

impl Descriptor {
    /// Creates one literal descriptor while keeping the table compact enough to audit.
    const fn new(
        offset: i16,
        offset_increment: i16,
        width_in_eights: u16,
        height: u16,
        animation_index: u8,
    ) -> Self {
        Self {
            offset,
            offset_increment,
            width_in_eights,
            height,
            animation_index,
        }
    }
}

/// Drawable rectangle and destination displacement for one animation layer.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct SpritePart {
    /// Top-left pixel in the unscaled moving-sprite sheet.
    pub(crate) source: SourcePoint,
    /// Unscaled source width.
    pub(crate) width: u32,
    /// Unscaled source height.
    pub(crate) height: u32,
    /// Unscaled horizontal displacement from the action's logical anchor cell.
    pub(crate) offset_x: i32,
    /// Unscaled vertical displacement from the action's logical anchor cell.
    pub(crate) offset_y: i32,
}

/// Ordered sprite layers needed to reconstruct one complete Murphy action frame.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct SpriteParts {
    /// Complete Murphy cell retained underneath an adjacent snap descriptor.
    pub(crate) retained: Option<SpritePart>,
    /// Main descriptor layer drawn for every Murphy action.
    pub(crate) primary: SpritePart,
    /// Second clipped Murphy layer drawn only during port traversal.
    pub(crate) secondary: Option<SpritePart>,
}

/// Returns the exact drawable source rectangles for one semantic Murphy action.
pub(crate) fn sprite_parts(action: MurphyAnimation, frame: u8) -> SpriteParts {
    // Planting spends its first half in the ordinary standing pose, its second
    // half in the original kneeling hold pose, and uses descriptor 49 only for
    // the final placement update.
    if action == MurphyAnimation::PlantRedDisk {
        let source = if frame < 32 {
            SourcePoint { x: 304, y: 132 }
        } else if frame < 64 {
            SourcePoint { x: 288, y: 132 }
        } else {
            SourcePoint { x: 256, y: 164 }
        };
        return SpriteParts {
            retained: None,
            primary: SpritePart {
                source,
                width: 16,
                height: 16,
                offset_x: 0,
                offset_y: 0,
            },
            secondary: None,
        };
    }

    let descriptor = MURPHY_DESCRIPTORS[descriptor_index(action)];
    let frame_index = usize::from(frame);
    let source = frame_coordinate(descriptor.animation_index, frame_index);
    let packed_offset = if matches!(action, MurphyAnimation::Port { .. }) {
        descriptor.offset
    } else {
        descriptor
            .offset
            .wrapping_add(descriptor.offset_increment.wrapping_mul(i16::from(frame)))
    };
    let (offset_x, offset_y) = decode_packed_offset(packed_offset);
    let primary = SpritePart {
        source,
        width: u32::from(descriptor.width_in_eights) * 8,
        height: u32::from(descriptor.height),
        offset_x,
        offset_y,
    };

    // A port descriptor reinterprets its increment as the fixed offset of a
    // second animation sequence. It never adds that field to the first layer.
    let secondary = if matches!(action, MurphyAnimation::Port { .. }) {
        let source = frame_coordinate(descriptor.animation_index + 1, frame_index);
        let (offset_x, offset_y) = decode_packed_offset(descriptor.offset_increment);
        Some(SpritePart {
            source,
            width: u32::from(descriptor.width_in_eights) * 8,
            height: u32::from(descriptor.height),
            offset_x,
            offset_y,
        })
    } else {
        None
    };

    // Snap descriptors draw only into the adjacent material cell. The DOS
    // level bitmap retained a complete Murphy picture in his own cell, so a
    // stateless renderer must explicitly reproduce that persistent underlay.
    let retained = if let MurphyAnimation::Snap { direction, target } = action {
        Some(snap_retained_part(direction, target))
    } else {
        None
    };

    SpriteParts {
        retained,
        primary,
        secondary,
    }
}

/// Returns the complete Murphy picture retained while an adjacent cell is snapped.
fn snap_retained_part(direction: Direction, target: MurphySnapTarget) -> SpritePart {
    // Base and Infotron handlers copied these direction-specific pictures into
    // Murphy's cell immediately before starting the adjacent-cell strip. Red
    // Disk handlers relied on the already persistent bitmap instead; the still
    // picture is the faithful deterministic reconstruction after Murphy rests.
    let source = match target {
        MurphySnapTarget::Base | MurphySnapTarget::Infotron => match direction {
            Direction::Up => SourcePoint { x: 160, y: 64 },
            Direction::Left => SourcePoint { x: 208, y: 16 },
            Direction::Down => SourcePoint { x: 176, y: 64 },
            Direction::Right => SourcePoint { x: 192, y: 16 },
        },
        MurphySnapTarget::RedDisk => SourcePoint { x: 304, y: 132 },
    };

    // The retained image replaces exactly Murphy's own cell. Directional
    // displacement belongs exclusively to the primary snap descriptor, which
    // is deliberately positioned over the neighboring material cell.
    SpritePart {
        source,
        width: 16,
        height: 16,
        offset_x: 0,
        offset_y: 0,
    }
}

/// Maps a typed action to the descriptor selected by the original direction handlers.
fn descriptor_index(action: MurphyAnimation) -> usize {
    match action {
        MurphyAnimation::Move {
            direction,
            target,
            looking_left,
        } => match (target, direction, looking_left) {
            (MurphyMoveTarget::Empty, Direction::Up, true) => 0,
            (MurphyMoveTarget::Empty, Direction::Up, false) => 1,
            (MurphyMoveTarget::Empty, Direction::Left, _) => 2,
            (MurphyMoveTarget::Empty, Direction::Down, true) => 3,
            (MurphyMoveTarget::Empty, Direction::Down, false) => 4,
            (MurphyMoveTarget::Empty, Direction::Right, _) => 5,
            (MurphyMoveTarget::Base, Direction::Up, true) => 7,
            (MurphyMoveTarget::Base, Direction::Up, false) => 8,
            (MurphyMoveTarget::Base, Direction::Left, _) => 9,
            (MurphyMoveTarget::Base, Direction::Down, true) => 10,
            (MurphyMoveTarget::Base, Direction::Down, false) => 11,
            (MurphyMoveTarget::Base, Direction::Right, _) => 12,
            (MurphyMoveTarget::Infotron, Direction::Up, true) => 17,
            (MurphyMoveTarget::Infotron, Direction::Up, false) => 18,
            (MurphyMoveTarget::Infotron, Direction::Left, _) => 19,
            (MurphyMoveTarget::Infotron, Direction::Down, true) => 20,
            (MurphyMoveTarget::Infotron, Direction::Down, false) => 21,
            (MurphyMoveTarget::Infotron, Direction::Right, _) => 22,
            (MurphyMoveTarget::RedDisk | MurphyMoveTarget::PlantedRedDisk, Direction::Up, true) => {
                33
            }
            (
                MurphyMoveTarget::RedDisk | MurphyMoveTarget::PlantedRedDisk,
                Direction::Up,
                false,
            ) => 34,
            (MurphyMoveTarget::RedDisk | MurphyMoveTarget::PlantedRedDisk, Direction::Left, _) => {
                35
            }
            (
                MurphyMoveTarget::RedDisk | MurphyMoveTarget::PlantedRedDisk,
                Direction::Down,
                true,
            ) => 36,
            (
                MurphyMoveTarget::RedDisk | MurphyMoveTarget::PlantedRedDisk,
                Direction::Down,
                false,
            ) => 37,
            (MurphyMoveTarget::RedDisk | MurphyMoveTarget::PlantedRedDisk, Direction::Right, _) => {
                38
            }
        },
        MurphyAnimation::Snap { direction, target } => match (target, direction) {
            (MurphySnapTarget::Base, Direction::Up) => 13,
            (MurphySnapTarget::Base, Direction::Left) => 14,
            (MurphySnapTarget::Base, Direction::Down) => 15,
            (MurphySnapTarget::Base, Direction::Right) => 16,
            (MurphySnapTarget::Infotron, Direction::Up) => 23,
            (MurphySnapTarget::Infotron, Direction::Left) => 24,
            (MurphySnapTarget::Infotron, Direction::Down) => 25,
            (MurphySnapTarget::Infotron, Direction::Right) => 26,
            (MurphySnapTarget::RedDisk, Direction::Up) => 39,
            (MurphySnapTarget::RedDisk, Direction::Left) => 40,
            (MurphySnapTarget::RedDisk, Direction::Down) => 41,
            (MurphySnapTarget::RedDisk, Direction::Right) => 42,
        },
        MurphyAnimation::Push { direction, target } => match (target, direction) {
            (MurphyPushTarget::Zonk, Direction::Left) => 27,
            (MurphyPushTarget::Zonk, Direction::Right) => 28,
            (MurphyPushTarget::YellowDisk, Direction::Up) => 43,
            (MurphyPushTarget::YellowDisk, Direction::Left) => 44,
            (MurphyPushTarget::YellowDisk, Direction::Down) => 45,
            (MurphyPushTarget::YellowDisk, Direction::Right) => 46,
            (MurphyPushTarget::OrangeDisk, Direction::Left) => 47,
            (MurphyPushTarget::OrangeDisk, Direction::Right) => 48,
            // Actor collision rules reject these combinations before an
            // animation can be constructed.
            (
                MurphyPushTarget::Zonk | MurphyPushTarget::OrangeDisk,
                Direction::Up | Direction::Down,
            ) => {
                unreachable!("Zonks and Orange Disks cannot be pushed vertically")
            }
        },
        MurphyAnimation::Port { direction } => match direction {
            Direction::Up => 29,
            Direction::Left => 30,
            Direction::Down => 31,
            Direction::Right => 32,
        },
        MurphyAnimation::Exit => 6,
        MurphyAnimation::PlantRedDisk => 49,
    }
}

/// Decodes the original quotient/remainder offset representation into pixels.
fn decode_packed_offset(offset: i16) -> (i32, i32) {
    // Rust and the original C implementation both truncate signed division
    // toward zero, so negative port and snap offsets retain their exact sides.
    let offset = i32::from(offset);
    ((offset % 122) * 8, offset / 122)
}

/// Returns one coordinate with a defensive final-frame clamp for internal misuse.
fn frame_coordinate(animation_index: u8, frame: usize) -> SourcePoint {
    let coordinates = match animation_index {
        0 => &FRAMES_0[..],
        1 => &FRAMES_1[..],
        2 => &FRAMES_2[..],
        3 => &FRAMES_3[..],
        4 => &FRAMES_4[..],
        5 => &FRAMES_5[..],
        6 => &FRAMES_6[..],
        7 => &FRAMES_7[..],
        8 => &FRAMES_8[..],
        9 => &FRAMES_9[..],
        10 => &FRAMES_10[..],
        11 => &FRAMES_11[..],
        12 => &FRAMES_12[..],
        13 => &FRAMES_13[..],
        14 => &FRAMES_14[..],
        15 => &FRAMES_15[..],
        16 => &FRAMES_16[..],
        17 => &FRAMES_17[..],
        18 => &FRAMES_18[..],
        19 => &FRAMES_19[..],
        20 => &FRAMES_20[..],
        21 => &FRAMES_21[..],
        22 => &FRAMES_22[..],
        23 => &FRAMES_23[..],
        24 => &FRAMES_24[..],
        25 => &FRAMES_25[..],
        27 => &FRAMES_27[..],
        28 => &FRAMES_28[..],
        29 => &FRAMES_29[..],
        30 => &FRAMES_30[..],
        31 => &FRAMES_31[..],
        32 => &FRAMES_32[..],
        33 => &FRAMES_33[..],
        _ => unreachable!("Murphy descriptors reference only known coordinate tables"),
    };
    coordinates[frame.min(coordinates.len() - 1)]
}

/// Literal transcription of the fifty descriptors in original table order.
const MURPHY_DESCRIPTORS: [Descriptor; 50] = [
    Descriptor::new(1708, -244, 2, 18, 0),
    Descriptor::new(1708, -244, 2, 18, 1),
    Descriptor::new(0, 0, 4, 16, 4),
    Descriptor::new(-1952, 244, 2, 18, 2),
    Descriptor::new(-1952, 244, 2, 18, 3),
    Descriptor::new(-2, 0, 4, 16, 5),
    Descriptor::new(0, 0, 2, 16, 6),
    Descriptor::new(1708, -244, 2, 18, 0),
    Descriptor::new(1708, -244, 2, 18, 1),
    Descriptor::new(0, 0, 4, 16, 7),
    Descriptor::new(-1952, 244, 2, 18, 2),
    Descriptor::new(-1952, 244, 2, 18, 3),
    Descriptor::new(-2, 0, 4, 16, 8),
    Descriptor::new(-1952, 0, 2, 16, 9),
    Descriptor::new(-2, 0, 2, 16, 9),
    Descriptor::new(1952, 0, 2, 16, 9),
    Descriptor::new(2, 0, 2, 16, 9),
    Descriptor::new(1708, -244, 2, 18, 0),
    Descriptor::new(1708, -244, 2, 18, 1),
    Descriptor::new(0, 0, 4, 16, 10),
    Descriptor::new(-1952, 244, 2, 18, 2),
    Descriptor::new(-1952, 244, 2, 18, 3),
    Descriptor::new(-2, 0, 4, 16, 11),
    Descriptor::new(-1952, 0, 2, 16, 12),
    Descriptor::new(-2, 0, 2, 16, 12),
    Descriptor::new(1952, 0, 2, 16, 12),
    Descriptor::new(2, 0, 2, 16, 12),
    Descriptor::new(-4, 0, 6, 16, 13),
    Descriptor::new(0, 0, 6, 16, 14),
    Descriptor::new(0, -3904, 2, 16, 19),
    Descriptor::new(0, -4, 2, 16, 15),
    Descriptor::new(0, 3904, 2, 16, 21),
    Descriptor::new(0, 4, 2, 16, 17),
    Descriptor::new(-244, -244, 2, 18, 0),
    Descriptor::new(-244, -244, 2, 18, 1),
    Descriptor::new(0, 0, 4, 16, 23),
    Descriptor::new(0, 244, 2, 18, 2),
    Descriptor::new(0, 244, 2, 18, 3),
    Descriptor::new(-2, 0, 4, 16, 24),
    Descriptor::new(-1952, 0, 2, 16, 25),
    Descriptor::new(-2, 0, 2, 16, 25),
    Descriptor::new(1952, 0, 2, 16, 25),
    Descriptor::new(2, 0, 2, 16, 25),
    Descriptor::new(-2196, -244, 2, 34, 28),
    Descriptor::new(-4, 0, 6, 16, 29),
    Descriptor::new(0, 244, 2, 34, 30),
    Descriptor::new(0, 0, 6, 16, 31),
    Descriptor::new(-4, 0, 6, 16, 32),
    Descriptor::new(0, 0, 6, 16, 33),
    Descriptor::new(0, 0, 2, 16, 27),
];

/// Shorthand for one literal unscaled source coordinate.
const fn point(x: i32, y: i32) -> SourcePoint {
    SourcePoint { x, y }
}

// These arrays intentionally preserve repeated coordinates: duplicates are
// timing data, not redundant artwork. In particular the forty-entry Exit strip
// holds each source picture for four updates and rightward Red Disk movement
// retains its historical ninth frame.
const FRAMES_0: [SourcePoint; 8] = [
    point(0, 66),
    point(0, 66),
    point(16, 66),
    point(16, 66),
    point(32, 66),
    point(32, 66),
    point(16, 66),
    point(16, 66),
];
const FRAMES_1: [SourcePoint; 8] = [
    point(48, 66),
    point(48, 66),
    point(64, 66),
    point(64, 66),
    point(80, 66),
    point(80, 66),
    point(64, 66),
    point(64, 66),
];
const FRAMES_2: [SourcePoint; 8] = [
    point(0, 64),
    point(0, 64),
    point(16, 64),
    point(16, 64),
    point(32, 64),
    point(32, 64),
    point(16, 64),
    point(16, 64),
];
const FRAMES_3: [SourcePoint; 8] = [
    point(48, 64),
    point(48, 64),
    point(64, 64),
    point(64, 64),
    point(80, 64),
    point(80, 64),
    point(64, 64),
    point(64, 64),
];
const FRAMES_4: [SourcePoint; 8] = [
    point(32, 32),
    point(64, 32),
    point(96, 32),
    point(128, 32),
    point(160, 32),
    point(192, 32),
    point(224, 32),
    point(256, 32),
];
const FRAMES_5: [SourcePoint; 8] = [
    point(288, 32),
    point(0, 48),
    point(32, 48),
    point(64, 48),
    point(96, 48),
    point(128, 48),
    point(160, 48),
    point(192, 48),
];
const FRAMES_6: [SourcePoint; 40] = [
    point(160, 64),
    point(160, 64),
    point(160, 64),
    point(160, 64),
    point(176, 64),
    point(176, 64),
    point(176, 64),
    point(176, 64),
    point(192, 64),
    point(192, 64),
    point(192, 64),
    point(192, 64),
    point(208, 64),
    point(208, 64),
    point(208, 64),
    point(208, 64),
    point(224, 64),
    point(224, 64),
    point(224, 64),
    point(224, 64),
    point(240, 64),
    point(240, 64),
    point(240, 64),
    point(240, 64),
    point(256, 64),
    point(256, 64),
    point(256, 64),
    point(256, 64),
    point(272, 64),
    point(272, 64),
    point(272, 64),
    point(272, 64),
    point(288, 64),
    point(288, 64),
    point(288, 64),
    point(288, 64),
    point(240, 0),
    point(240, 0),
    point(240, 0),
    point(240, 0),
];
const FRAMES_7: [SourcePoint; 8] = [
    point(0, 0),
    point(32, 0),
    point(64, 0),
    point(96, 0),
    point(128, 0),
    point(160, 0),
    point(192, 0),
    point(224, 0),
];
const FRAMES_8: [SourcePoint; 8] = [
    point(256, 0),
    point(288, 0),
    point(0, 16),
    point(32, 16),
    point(64, 16),
    point(96, 16),
    point(128, 16),
    point(160, 16),
];
const FRAMES_9: [SourcePoint; 8] = [
    point(256, 84),
    point(272, 84),
    point(288, 84),
    point(304, 84),
    point(256, 100),
    point(272, 100),
    point(288, 100),
    point(304, 148),
];
const FRAMES_10: [SourcePoint; 8] = [
    point(0, 212),
    point(32, 212),
    point(64, 212),
    point(96, 212),
    point(128, 212),
    point(160, 212),
    point(192, 212),
    point(224, 212),
];
const FRAMES_11: [SourcePoint; 8] = [
    point(256, 212),
    point(288, 212),
    point(0, 228),
    point(32, 228),
    point(64, 228),
    point(96, 228),
    point(128, 228),
    point(160, 228),
];
const FRAMES_12: [SourcePoint; 7] = [
    point(192, 148),
    point(208, 148),
    point(224, 148),
    point(256, 148),
    point(272, 148),
    point(288, 148),
    point(304, 148),
];
const FRAMES_13: [SourcePoint; 8] = [
    point(0, 116),
    point(48, 116),
    point(96, 116),
    point(144, 116),
    point(192, 116),
    point(240, 116),
    point(0, 132),
    point(48, 132),
];
const FRAMES_14: [SourcePoint; 8] = [
    point(96, 132),
    point(144, 132),
    point(192, 132),
    point(240, 132),
    point(0, 148),
    point(48, 148),
    point(96, 148),
    point(144, 148),
];
const FRAMES_15: [SourcePoint; 8] = [
    point(48, 32),
    point(80, 32),
    point(112, 32),
    point(144, 32),
    point(176, 32),
    point(208, 32),
    point(240, 32),
    point(272, 32),
];
const FRAMES_16: [SourcePoint; 8] = [
    point(32, 32),
    point(64, 32),
    point(96, 32),
    point(128, 32),
    point(160, 32),
    point(192, 32),
    point(224, 32),
    point(256, 32),
];
const FRAMES_17: [SourcePoint; 8] = [
    point(288, 32),
    point(0, 48),
    point(32, 48),
    point(64, 48),
    point(96, 48),
    point(128, 48),
    point(160, 48),
    point(192, 48),
];
const FRAMES_18: [SourcePoint; 8] = [
    point(304, 32),
    point(16, 48),
    point(48, 48),
    point(80, 48),
    point(112, 48),
    point(144, 48),
    point(176, 48),
    point(208, 48),
];
const FRAMES_19: [SourcePoint; 8] = [
    point(304, 134),
    point(304, 136),
    point(304, 138),
    point(304, 140),
    point(304, 142),
    point(304, 144),
    point(304, 146),
    point(304, 148),
];
const FRAMES_20: [SourcePoint; 8] = [
    point(304, 118),
    point(304, 120),
    point(304, 122),
    point(304, 124),
    point(304, 126),
    point(304, 128),
    point(304, 130),
    point(304, 132),
];
const FRAMES_21: [SourcePoint; 8] = [
    point(304, 130),
    point(304, 128),
    point(304, 126),
    point(304, 124),
    point(304, 122),
    point(304, 120),
    point(304, 118),
    point(304, 116),
];
const FRAMES_22: [SourcePoint; 8] = [
    point(304, 146),
    point(304, 144),
    point(304, 142),
    point(304, 140),
    point(304, 138),
    point(304, 136),
    point(304, 134),
    point(304, 132),
];
const FRAMES_23: [SourcePoint; 8] = [
    point(128, 260),
    point(160, 260),
    point(192, 260),
    point(224, 260),
    point(256, 260),
    point(288, 260),
    point(288, 276),
    point(288, 292),
];
const FRAMES_24: [SourcePoint; 9] = [
    point(192, 308),
    point(224, 308),
    point(256, 308),
    point(288, 308),
    point(288, 308),
    point(288, 324),
    point(288, 340),
    point(192, 356),
    point(224, 356),
];
const FRAMES_25: [SourcePoint; 8] = [
    point(256, 164),
    point(272, 164),
    point(288, 164),
    point(304, 164),
    point(256, 180),
    point(272, 180),
    point(288, 180),
    point(304, 180),
];
const FRAMES_27: [SourcePoint; 1] = [point(256, 164)];
const FRAMES_28: [SourcePoint; 8] = [
    point(304, 406),
    point(304, 406),
    point(304, 406),
    point(304, 406),
    point(304, 406),
    point(304, 406),
    point(304, 406),
    point(304, 406),
];
const FRAMES_29: [SourcePoint; 8] = [
    point(0, 324),
    point(48, 324),
    point(96, 324),
    point(144, 324),
    point(192, 324),
    point(240, 324),
    point(0, 340),
    point(48, 340),
];
const FRAMES_30: [SourcePoint; 8] = [
    point(288, 406),
    point(288, 406),
    point(288, 406),
    point(288, 406),
    point(288, 406),
    point(288, 406),
    point(288, 406),
    point(288, 406),
];
const FRAMES_31: [SourcePoint; 8] = [
    point(96, 340),
    point(144, 340),
    point(192, 340),
    point(240, 340),
    point(0, 356),
    point(48, 356),
    point(96, 356),
    point(144, 356),
];
const FRAMES_32: [SourcePoint; 8] = [
    point(0, 276),
    point(48, 276),
    point(96, 276),
    point(144, 276),
    point(192, 276),
    point(240, 276),
    point(0, 292),
    point(48, 292),
];
const FRAMES_33: [SourcePoint; 8] = [
    point(96, 292),
    point(144, 292),
    point(192, 292),
    point(240, 292),
    point(0, 308),
    point(48, 308),
    point(96, 308),
    point(144, 308),
];

#[cfg(test)]
mod tests {
    //! Descriptor checks for the most error-prone direction and target choices.

    use super::{SourcePoint, sprite_parts};
    use crate::actors::{
        Direction, MurphyAnimation, MurphyMoveTarget, MurphyPushTarget, MurphySnapTarget,
    };

    /// Verifies vertical Base movement retains the horizontal look variant.
    #[test]
    fn vertical_base_frames_depend_on_horizontal_facing() {
        let left = sprite_parts(
            MurphyAnimation::Move {
                direction: Direction::Up,
                target: MurphyMoveTarget::Base,
                looking_left: true,
            },
            0,
        );
        let right = sprite_parts(
            MurphyAnimation::Move {
                direction: Direction::Up,
                target: MurphyMoveTarget::Base,
                looking_left: false,
            },
            0,
        );
        assert_eq!(left.primary.source, SourcePoint { x: 0, y: 66 });
        assert_eq!(right.primary.source, SourcePoint { x: 48, y: 66 });
    }

    /// Verifies target-specific snapping does not collapse onto one pose.
    #[test]
    fn snap_targets_select_distinct_coordinate_tables() {
        let base = sprite_parts(
            MurphyAnimation::Snap {
                direction: Direction::Left,
                target: MurphySnapTarget::Base,
            },
            0,
        );
        let infotron = sprite_parts(
            MurphyAnimation::Snap {
                direction: Direction::Left,
                target: MurphySnapTarget::Infotron,
            },
            0,
        );
        assert_ne!(base.primary.source, infotron.primary.source);
    }

    /// Verifies every snap reconstructs Murphy while its descriptor replaces the target cell.
    #[test]
    fn snap_frames_retain_a_complete_murphy_in_his_own_cell() {
        let directional_poses = [
            (Direction::Up, SourcePoint { x: 160, y: 64 }),
            (Direction::Left, SourcePoint { x: 208, y: 16 }),
            (Direction::Down, SourcePoint { x: 176, y: 64 }),
            (Direction::Right, SourcePoint { x: 192, y: 16 }),
        ];

        for target in [MurphySnapTarget::Base, MurphySnapTarget::Infotron] {
            for (direction, expected_source) in directional_poses {
                let parts = sprite_parts(MurphyAnimation::Snap { direction, target }, 0);
                let retained = parts
                    .retained
                    .expect("Base and Infotron snaps must retain Murphy");

                assert_eq!(retained.source, expected_source);
                assert_eq!((retained.width, retained.height), (16, 16));
                assert_eq!((retained.offset_x, retained.offset_y), (0, 0));
                assert_ne!(
                    (parts.primary.offset_x, parts.primary.offset_y),
                    (0, 0),
                    "the animated descriptor must remain in the adjacent cell"
                );
            }
        }

        for direction in [
            Direction::Up,
            Direction::Left,
            Direction::Down,
            Direction::Right,
        ] {
            let retained = sprite_parts(
                MurphyAnimation::Snap {
                    direction,
                    target: MurphySnapTarget::RedDisk,
                },
                0,
            )
            .retained
            .expect("Red Disk snaps must retain Murphy");

            assert_eq!(retained.source, SourcePoint { x: 304, y: 132 });
            assert_eq!((retained.width, retained.height), (16, 16));
            assert_eq!((retained.offset_x, retained.offset_y), (0, 0));
        }
    }

    /// Verifies wide pushes use the full three-tile composite width.
    #[test]
    fn horizontal_push_frames_span_three_original_tiles() {
        let parts = sprite_parts(
            MurphyAnimation::Push {
                direction: Direction::Right,
                target: MurphyPushTarget::OrangeDisk,
            },
            0,
        );
        assert_eq!(parts.primary.width, 48);
    }

    /// Verifies ports retain a separately positioned opposite endpoint layer.
    #[test]
    fn port_frames_have_two_layers() {
        let first = sprite_parts(
            MurphyAnimation::Port {
                direction: Direction::Up,
            },
            0,
        );
        let last = sprite_parts(
            MurphyAnimation::Port {
                direction: Direction::Up,
            },
            7,
        );
        assert_eq!(
            first.secondary.expect("port needs opposite layer").offset_y,
            -32
        );
        assert_eq!(first.primary.offset_y, last.primary.offset_y);
    }
}
