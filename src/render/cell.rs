//! Sprite selection from previous/current typed cells; the simulation emits no graphics.
//!
//! A selector borrows the two frame buffers and returns at most two atlas parts.
//! Transfers keep the callback's old anchor, while the neighboring current cell
//! supplies the new phase. The renderer consumes these values immediately.

use super::{
    SourcePoint, SpritePart, bug_sprite_part, electron_sprite_part, explosion_sprite_part,
    fixed_tile_source, infotron_sprite_part, orange_sprite_part, snik_snak_sprite_part,
    sprite_parts, zonk_sprite_part,
};
use crate::{
    actors::{
        Actor, Bug, Direction, Empty, Frame, Murphy, MurphyAnimation, Position, empty::Reservation,
        murphy::MurphyPhase, orange_disk::OrangePhase, rounded::RoundedPhase,
    },
    game::Board,
};

/// One opaque rectangle from a decoded atlas, with a cell-relative destination.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Sprite {
    /// Original stationary tile artwork.
    Fixed(SpritePart),
    /// Original animation rectangle, including its black erase pixels.
    Moving(SpritePart),
    /// One preassembled Terminal tile at its current scrolling phase.
    Terminal(SpritePart),
}

/// An actor resolves to no change, one rectangle, or its two-part composite.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) enum Sprites {
    /// This cell contributes no bitmap change on this frame.
    #[default]
    None,
    /// A stationary tile or ordinary animation rectangle.
    One(Sprite),
    /// A retained snap pose plus target, or the two port endpoints.
    Two(Sprite, Sprite),
}

impl Sprites {
    /// Visits the bounded parts in their original opaque-copy order without allocation.
    pub(super) fn iter(self) -> impl Iterator<Item = Sprite> {
        match self {
            Self::None => [None, None],
            Self::One(first) => [Some(first), None],
            Self::Two(first, second) => [Some(first), Some(second)],
        }
        .into_iter()
        .flatten()
    }

    /// Reanchors a transferred actor without changing its atlas rectangle.
    fn translated(self, direction: Direction) -> Self {
        let translate = |sprite| {
            let (dx, dy) = match direction {
                Direction::Up => (0, -16),
                Direction::Down => (0, 16),
                Direction::Left => (-16, 0),
                Direction::Right => (16, 0),
            };
            let move_part = |mut part: SpritePart| {
                part.offset_x += dx;
                part.offset_y += dy;
                part
            };
            match sprite {
                Sprite::Fixed(p) => Sprite::Fixed(move_part(p)),
                Sprite::Moving(p) => Sprite::Moving(move_part(p)),
                Sprite::Terminal(p) => Sprite::Terminal(move_part(p)),
            }
        };
        match self {
            Self::None => Self::None,
            Self::One(a) => Self::One(translate(a)),
            Self::Two(a, b) => Self::Two(translate(a), translate(b)),
        }
    }
}

/// Selects one complete fixed tile without interpreting black as transparency.
pub(super) fn fixed_tile(tile: u8) -> Sprite {
    let source = fixed_tile_source(tile);
    Sprite::Fixed(SpritePart {
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

/// Converts one Murphy descriptor to its one or two original rectangles.
fn murphy_parts(action: MurphyAnimation, frame: u8) -> Sprites {
    let parts = sprite_parts(action, frame);
    match (parts.retained, parts.secondary) {
        (Some(retained), _) => {
            Sprites::Two(Sprite::Moving(retained), Sprite::Moving(parts.primary))
        }
        (None, Some(second)) => Sprites::Two(Sprite::Moving(parts.primary), Sprite::Moving(second)),
        (None, None) => Sprites::One(Sprite::Moving(parts.primary)),
    }
}

/// Selects an actor's present artwork for initialization or a changed stationary cell.
pub(super) fn actor_sprites(actor: &Actor) -> Sprites {
    let part = match actor {
        Actor::Empty(Empty::Space) | Actor::InvisibleWall(_) => return Sprites::One(fixed_tile(0)),
        Actor::Empty(Empty::Reserved(_)) => return Sprites::None,
        Actor::Zonk(a) => zonk_sprite_part(a.phase()),
        Actor::Infotron(a) => infotron_sprite_part(a.phase()),
        Actor::OrangeDisk(a) => match a.phase() {
            OrangePhase::Falling(f) => Some(orange_sprite_part(f)),
            _ => None,
        },
        Actor::Murphy(actor) => return player_sprite(actor),
        Actor::SnikSnak(a) => Some(snik_snak_sprite_part(a.phase())),
        Actor::Electron(a) => Some(electron_sprite_part(a.phase())),
        Actor::Explosion(a) => Some(explosion_sprite_part(a.residue(), a.frame())),
        Actor::Bug(Bug::Active(f)) => Some(bug_sprite_part(*f)),
        Actor::Bug(Bug::Dormant(_)) => return Sprites::One(fixed_tile(2)),
        Actor::Terminal(a) => {
            return Sprites::One(Sprite::Terminal(SpritePart {
                source: SourcePoint {
                    x: i32::from(a.screen_frame()) * 16,
                    y: 0,
                },
                width: 16,
                height: 16,
                offset_x: 0,
                offset_y: 0,
            }));
        }
        _ => None,
    };
    Sprites::One(part.map_or_else(|| fixed_tile(actor.tile_code()), Sprite::Moving))
}

/// Maps Murphy's current legal phase to its bounded sprite composite.
fn player_sprite(actor: &Murphy) -> Sprites {
    if let MurphyPhase::PreparingPush { action, .. } = actor.phase() {
        return Sprites::One(Sprite::Moving(SpritePart {
            source: SourcePoint {
                x: if action.direction() == Direction::Left {
                    64
                } else {
                    97
                },
                y: 132,
            },
            width: 16,
            height: 16,
            offset_x: 0,
            offset_y: 0,
        }));
    }
    actor
        .sprite_pose()
        .map_or(Sprites::One(fixed_tile(3)), |(action, frame)| {
            murphy_parts(action, frame)
        })
}

/// Locates the one player in a complete frame without a separate mutable actor index.
fn murphy(board: &Board) -> Option<(Position, &Murphy)> {
    board
        .cells()
        .iter()
        .enumerate()
        .find_map(|(i, cell)| match cell.actor() {
            Actor::Murphy(actor) => Some((board.position(i).unwrap(), actor)),
            _ => None,
        })
}

/// Resolves Murphy first, retaining the previous action's anchor on completion.
pub(super) fn murphy_sprites(previous: &Board, current: &Board) -> Option<(Position, Sprites)> {
    let old = murphy(previous);
    let new = murphy(current);
    match (old, new) {
        (Some((position, before)), next) => {
            // Completion belongs to the old action even if its current cell is
            // empty or now holds another actor. No synthetic Actor is constructed.
            let completed = matches!(
                (before.phase(), next.map(|(_, a)| a.phase())),
                (
                    MurphyPhase::Snapping(_)
                        | MurphyPhase::Pushing { .. }
                        | MurphyPhase::CrossingPort { .. },
                    Some(MurphyPhase::Ready)
                ) | (MurphyPhase::Exiting(_), None)
            );
            if completed {
                let (action, _) = before
                    .sprite_pose()
                    .expect("finite actions have a descriptor");
                return Some((position, murphy_parts(action, action.frame_count() - 1)));
            }
            let (destination, after) = next?;
            match (before.phase(), after.phase()) {
                (MurphyPhase::PreparingPush { .. }, MurphyPhase::PreparingPush { .. })
                | (MurphyPhase::Ready | MurphyPhase::Resuming(_), MurphyPhase::Ready) => None,
                _ if before == after && position == destination => None,
                _ => Some((destination, player_sprite(after))),
            }
        }
        (None, Some((position, actor))) => Some((position, player_sprite(actor))),
        (None, None) => None,
    }
}

/// Reads the shared rounded phase without accepting phases from unrelated actors.
fn rounded(actor: &Actor) -> Option<RoundedPhase> {
    match actor {
        Actor::Zonk(a) => Some(a.phase()),
        Actor::Infotron(a) => Some(a.phase()),
        _ => None,
    }
}

/// Resolves a rounded picture with the concrete actor's original atlas coordinates.
fn rounded_sprite(actor: &Actor, phase: RoundedPhase) -> Sprites {
    let part = match actor {
        Actor::Zonk(_) => zonk_sprite_part(phase),
        Actor::Infotron(_) => infotron_sprite_part(phase),
        _ => None,
    };
    part.map_or(Sprites::None, |p| Sprites::One(Sprite::Moving(p)))
}

/// Applies a cardinal offset within the two buffers' common board dimensions.
fn neighbor(board: &Board, position: Position, direction: Direction) -> Option<Position> {
    let (x, y) = match direction {
        Direction::Up => (position.x, position.y.checked_sub(1)?),
        Direction::Down => (position.x, position.y + 1),
        Direction::Left => (position.x.checked_sub(1)?, position.y),
        Direction::Right => (position.x + 1, position.y),
    };
    let result = Position::new(x, y);
    board.index(result).map(|_| result)
}

/// Identifies destruction of a reservation's owner rather than normal source release.
fn owner_exploded(
    previous: &Board,
    current: &Board,
    position: Position,
    reservation: Reservation,
) -> bool {
    let directions: &[Direction] = match reservation {
        Reservation::Vacating {
            direction: Direction::Down,
            ..
        } => &[Direction::Down],
        Reservation::RollingSource(d) | Reservation::RoundedCorner(d) => match d {
            crate::actors::Horizontal::Left => &[Direction::Left],
            crate::actors::Horizontal::Right => &[Direction::Right],
        },
        Reservation::RoundedDestination | Reservation::RoundedContinuation => &[Direction::Up],
        Reservation::RoundedSide => &[Direction::Left, Direction::Right],
        _ => &[],
    };
    directions.iter().any(|&direction| {
        neighbor(previous, position, direction).is_some_and(|owner| {
            rounded(previous.state(owner).unwrap().actor()).is_some()
                && matches!(current.state(owner).unwrap().actor(), Actor::Explosion(_))
        })
    })
}

/// Resolves one non-player cell against its previous state and neighboring transfers.
pub(super) fn cell_sprites(
    previous: &Board,
    current: &Board,
    position: Position,
    freeze_zonks: bool,
) -> Sprites {
    let before = previous.state(position).unwrap().actor();
    let after = current.state(position).unwrap().actor();
    // Murphy may collect an active Infotron downward before the object pass.
    // A replaced actor contributes no sprite after that player-first update.
    if matches!(after, Actor::Murphy(_)) {
        return Sprites::None;
    }
    // An explosion owns its cell even when an earlier actor occupied it at frame
    // start. Its current bounded phase supplies the complete replacement sprite.
    if matches!(after, Actor::Explosion(_)) {
        return if before == after {
            Sprites::None
        } else {
            actor_sprites(after)
        };
    }
    if let Some(phase) = rounded(before) {
        let paused = freeze_zonks
            && matches!(before, Actor::Zonk(_))
            && !matches!(
                phase,
                RoundedPhase::Rolling { .. } | RoundedPhase::Falling(_)
            );
        if paused {
            return Sprites::None;
        }
        match phase {
            RoundedPhase::Rolling { .. }
            | RoundedPhase::Falling(_)
            | RoundedPhase::PreparingRoll { .. } => return rounded_sprite(before, phase),
            RoundedPhase::Momentum if matches!(after, Actor::Empty(_)) => {
                return rounded_sprite(before, RoundedPhase::Falling(Frame::first()))
                    .translated(Direction::Down);
            }
            _ => {}
        }
    }
    // Enemy and Orange Disk transfers are evaluated at the source's row-major
    // slot. Their new destination must not contribute a duplicate sprite later.
    if matches!(
        after,
        Actor::Empty(Empty::Reserved(_)) | Actor::Empty(Empty::Space)
    ) {
        let transfer = match (before, after) {
            (Actor::SnikSnak(_), Actor::Empty(Empty::Reserved(Reservation::SnikSnakSource(d)))) => {
                Some(*d)
            }
            (Actor::Electron(_), Actor::Empty(Empty::Reserved(Reservation::ElectronSource(d)))) => {
                Some(*d)
            }
            (Actor::OrangeDisk(a), _) if matches!(a.phase(), OrangePhase::Falling(_)) => {
                Some(Direction::Down)
            }
            _ => None,
        };
        if let Some(direction) = transfer
            && let Some(destination) = neighbor(current, position, direction)
        {
            let actor = current.state(destination).unwrap().actor();
            if matches!(
                (before, actor),
                (Actor::SnikSnak(_), Actor::SnikSnak(_))
                    | (Actor::Electron(_), Actor::Electron(_))
                    | (Actor::OrangeDisk(_), Actor::OrangeDisk(_))
            ) {
                return actor_sprites(actor).translated(direction);
            }
        }
    }
    match (before, after) {
        (_, Actor::Empty(Empty::Reserved(_))) => Sprites::None,
        (Actor::Empty(Empty::Reserved(reservation)), Actor::Empty(Empty::Space)) => {
            if owner_exploded(previous, current, position, *reservation) {
                Sprites::One(fixed_tile(0))
            } else {
                Sprites::None
            }
        }
        (Actor::Explosion(_), Actor::Empty(Empty::Space)) => Sprites::One(fixed_tile(0)),
        (_, Actor::Empty(Empty::Space)) => Sprites::None,
        (_, Actor::Zonk(_) | Actor::Infotron(_)) => match rounded(after).unwrap() {
            RoundedPhase::PreparingRoll { direction, frame } if frame.index() == 1 => {
                rounded_sprite(
                    after,
                    RoundedPhase::PreparingRoll {
                        direction,
                        frame: Frame::first(),
                    },
                )
            }
            RoundedPhase::Resting if matches!(before, Actor::Explosion(_)) => actor_sprites(after),
            _ => Sprites::None,
        },
        (Actor::Bug(Bug::Dormant(_)), Actor::Bug(Bug::Dormant(_) | Bug::Held)) => Sprites::None,
        (_, actor) if actor.is_held() => Sprites::None,
        (Actor::Terminal(a), Actor::Terminal(b)) if a.screen_frame() == b.screen_frame() => {
            Sprites::None
        }
        (Actor::SnikSnak(_), Actor::SnikSnak(_)) | (Actor::Electron(_), Actor::Electron(_)) => {
            if before == after {
                Sprites::None
            } else {
                actor_sprites(after)
            }
        }
        (_, Actor::SnikSnak(_) | Actor::Electron(_)) => Sprites::None,
        (_, Actor::OrangeDisk(_)) if !matches!(before, Actor::OrangeDisk(_)) => Sprites::None,
        (Actor::OrangeDisk(a), Actor::OrangeDisk(b))
            if !matches!(a.phase(), OrangePhase::Falling(_))
                && !matches!(b.phase(), OrangePhase::Falling(_)) =>
        {
            Sprites::None
        }
        _ if before == after => Sprites::None,
        _ => actor_sprites(after),
    }
}

#[cfg(test)]
mod tests;
