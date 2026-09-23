//! State-pair boundaries that cannot be expressed by current-cell artwork alone.

use super::*;
use crate::actors::{Explosion, ExplosionResidue, Horizontal, Infotron, OrangeDisk, State, Zonk};

/// Builds two equal-sized frames with explicitly chosen neighboring actor states.
fn board(cells: &[(Position, Actor)]) -> Board {
    let mut states = vec![State::empty(); 6 * 6];
    for (position, actor) in cells {
        states[position.y * 6 + position.x] = State::new(actor.clone());
    }
    Board::new(6, 6, states).unwrap()
}

/// Murphy's earlier callback can consume an active Infotron before its old slot runs.
#[test]
fn collected_infotron_cannot_contribute_a_sprite_after_player_replacement() {
    let position = Position::new(2, 2);
    for phase in [
        RoundedPhase::Resting,
        RoundedPhase::AwaitingFall,
        RoundedPhase::Falling(Frame::first()),
    ] {
        let previous = board(&[(position, Actor::Infotron(Infotron::from_phase(phase)))]);
        let current = board(&[(position, Actor::Murphy(Murphy::new()))]);
        assert_eq!(
            cell_sprites(&previous, &current, position, false),
            Sprites::None
        );
        let (anchor, parts) = murphy_sprites(&previous, &current).unwrap();
        assert_eq!(anchor, position);
        assert_eq!(parts.iter().count(), 1);
    }
}

/// Identical preparation cells can repeat a picture, but freeze pauses only Zonks.
#[test]
fn frozen_preparation_and_repeating_preparation_have_distinct_cell_results() {
    let position = Position::new(2, 2);
    for direction in [Horizontal::Left, Horizontal::Right] {
        let preparing = RoundedPhase::PreparingRoll {
            direction,
            frame: Frame::last(),
        };
        let zonk = Actor::Zonk(Zonk::from_phase(preparing));
        let infotron = Actor::Infotron(Infotron::from_phase(preparing));
        for actor in [zonk, infotron] {
            let state = board(&[(position, actor.clone())]);
            let parts = cell_sprites(&state, &state, position, false);
            assert_eq!(
                parts.iter().count(),
                1,
                "blocked preparation repeats picture one"
            );
            assert_eq!(
                cell_sprites(&state, &state, position, true),
                if matches!(actor, Actor::Zonk(_)) {
                    Sprites::None
                } else {
                    parts
                }
            );
        }
    }
    // Freeze never interrupts an already reserved downward transfer.
    let previous = board(&[(
        position,
        Actor::Zonk(Zonk::from_phase(RoundedPhase::Falling(Frame::first()))),
    )]);
    let current = board(&[(
        position,
        Actor::Zonk(Zonk::from_phase(RoundedPhase::Falling(
            Frame::new(1).unwrap(),
        ))),
    )]);
    assert_eq!(
        cell_sprites(&previous, &current, position, true),
        cell_sprites(&previous, &current, position, false)
    );
    assert_eq!(
        cell_sprites(&previous, &current, position, true)
            .iter()
            .count(),
        1
    );
}

/// A source release changes occupancy; a destroyed owner instead clears its footprint.
#[test]
fn cell_pairs_distinguish_normal_source_release_from_destroyed_ownership() {
    let source = Position::new(2, 2);
    let destination = Position::new(3, 2);
    let corner = Actor::Empty(Empty::Reserved(Reservation::RoundedCorner(
        Horizontal::Right,
    )));
    let previous = board(&[
        (source, corner),
        (
            destination,
            Actor::Zonk(Zonk::from_phase(RoundedPhase::Rolling {
                direction: Horizontal::Right,
                frame: Frame::new(5).unwrap(),
            })),
        ),
    ]);
    let released = board(&[(
        destination,
        Actor::Zonk(Zonk::from_phase(RoundedPhase::Rolling {
            direction: Horizontal::Right,
            frame: Frame::new(6).unwrap(),
        })),
    )]);
    assert_eq!(
        cell_sprites(&previous, &released, source, false),
        Sprites::None
    );
    let blast = Actor::Explosion(Explosion::new(ExplosionResidue::Empty));
    let destroyed = board(&[(destination, blast.clone())]);
    assert_eq!(
        cell_sprites(&previous, &destroyed, source, false),
        Sprites::One(fixed_tile(0))
    );
    // When both cells are inside the wave, the source belongs to its explosion.
    let covered = board(&[(source, blast.clone()), (destination, blast.clone())]);
    assert_eq!(
        cell_sprites(&previous, &covered, source, false),
        actor_sprites(&blast)
    );
}

/// Falling transfers resolve at their old callback anchor and never duplicate at the new cell.
#[test]
fn orange_transfer_contributes_once_whether_it_continues_or_lands() {
    let source = Position::new(2, 2);
    let destination = Position::new(2, 3);
    let previous = board(&[
        (
            source,
            Actor::OrangeDisk(OrangeDisk::from_phase(OrangePhase::Falling(Frame::last()))),
        ),
        (
            destination,
            Actor::Empty(Empty::Reserved(Reservation::RoundedDestination)),
        ),
    ]);
    for phase in [OrangePhase::Falling(Frame::first()), OrangePhase::Resting] {
        let disk = Actor::OrangeDisk(OrangeDisk::from_phase(phase));
        let current = board(&[(destination, disk.clone())]);
        assert_eq!(
            cell_sprites(&previous, &current, source, false),
            actor_sprites(&disk).translated(Direction::Down)
        );
        assert_eq!(
            cell_sprites(&previous, &current, destination, false),
            Sprites::None
        );
    }
}
