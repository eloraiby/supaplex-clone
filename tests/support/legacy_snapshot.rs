// Test-only semantic serialization. Historical labels remain for unchanged phases;
// corrected rounded preparation and reservation phases have explicit encodings.
// These labels are never stored in board cells or consumed by rendering.
use self::model::{Actor, Bug, Empty, State, enemy::EnemyPhase, rounded::RoundedPhase};
use std::borrow::Cow;

/// Historical presentation bytes used solely to compare existing test fixtures.
pub struct Snapshot {
    /// Original serialized label; static values borrow rather than allocate.
    label: Cow<'static, str>,
    /// Visible picture encoded by the old fixture format.
    frame: u8,
    /// Original strip length encoded by that format.
    count: u8,
}

impl Snapshot {
    /// Packages fixture bytes without making an executable animation object.
    fn new(label: impl Into<Cow<'static, str>>, frame: u8, count: u8) -> Self {
        Self { label: label.into(), frame, count }
    }

    /// Returns the historical fixture label.
    pub fn label(&self) -> &str { &self.label }

    /// Returns the fixture's picture index.
    pub fn frame(&self) -> u8 { self.frame }

    /// Returns the fixture's strip length.
    pub fn frame_count(&self) -> u8 { self.count }
}

/// Provides the old fixture encoding without restoring an animation API to State.
pub trait SnapshotExt {
    /// Serializes the cell's real typed phase for a historical assertion.
    fn snapshot(&self) -> Snapshot;
}

/// Encodes rounded phases using the exact labels captured before this refactor.
fn rounded(phase: RoundedPhase, arming_label: &'static str) -> Snapshot {
    match phase {
        RoundedPhase::Resting | RoundedPhase::Momentum => Snapshot::new("Idle", 0, 1),
        RoundedPhase::AwaitingFall => Snapshot::new(arming_label, 0, 1),
        RoundedPhase::PreparingRoll { direction, frame } => Snapshot::new(format!("RoundedPreRoll({:?})", direction.direction()), frame.index(), 2),
        RoundedPhase::Rolling { direction, frame } => Snapshot::new(format!("Rolling({:?})", direction.direction()), frame.index(), 8),
        RoundedPhase::Falling(frame) => Snapshot::new("Moving(Down)", frame.index(), 8),
        RoundedPhase::Held => Snapshot::new("MurphyPushTarget", 0, 1),
    }
}

/// Encodes an enemy's bounded turn or move with its original sprite-family name.
fn enemy(phase: EnemyPhase, family: &str) -> Snapshot {
    match phase {
        EnemyPhase::Turning { turn, frame } => Snapshot::new(format!("{family}Turn({turn:?})"), frame.index(), 8),
        EnemyPhase::Moving { direction, frame } => Snapshot::new(format!("{family}Move({direction:?})"), frame.index(), 8),
    }
}

impl SnapshotExt for State {
    /// Reads only typed actor data; no generic animation state is constructed.
    fn snapshot(&self) -> Snapshot {
        use model::{empty::Reservation, orange_disk::OrangePhase};
        match self.actor() {
            Actor::Empty(Empty::Space) => Snapshot::new("Idle", 0, 1),
            Actor::Empty(Empty::Reserved(reservation)) => match reservation {
                Reservation::RollingSource(direction) => Snapshot::new(format!("RollingSource({direction:?})"), 0, 1),
                Reservation::RoundedCorner(direction) => Snapshot::new(format!("RoundedCorner({direction:?})"), 0, 1),
                Reservation::RoundedContinuation => Snapshot::new("RoundedContinuation", 0, 1),
                Reservation::Vacating { direction, duration } => Snapshot::new(format!("Vacating({direction:?})"), 0, duration.frames()),
                Reservation::SnikSnakSource(direction) => Snapshot::new(format!("SnikSnakVacating({direction:?})"), 0, 1),
                Reservation::ElectronSource(direction) => Snapshot::new(format!("ElectronVacating({direction:?})"), 0, 1),
                Reservation::MurphyDestination => Snapshot::new("MurphyDestination", 0, 1),
                Reservation::RoundedSide => Snapshot::new("RoundedSide", 0, 1),
                Reservation::RoundedDestination => Snapshot::new("RoundedDestination", 0, 1),
            },
            Actor::Zonk(actor) => rounded(actor.phase(), "ZonkPreFall"),
            Actor::Infotron(actor) => rounded(actor.phase(), "InfotronPreFall"),
            Actor::SnikSnak(actor) => enemy(actor.phase(), "SnikSnak"),
            Actor::Electron(actor) => enemy(actor.phase(), "Electron"),
            Actor::OrangeDisk(actor) => match actor.phase() {
                OrangePhase::Resting => Snapshot::new("Idle", 0, 1),
                OrangePhase::AwaitingFall(frame) => Snapshot::new("OrangePreFall", frame.index(), 2),
                OrangePhase::Falling(frame) => Snapshot::new("OrangeFalling", frame.index(), 8),
                OrangePhase::Fuse(frame) => Snapshot::new("OrangeDiskFuse", frame.index(), 6),
                OrangePhase::Held => Snapshot::new("MurphyPushTarget", 0, 1),
            },
            Actor::Murphy(actor) => match actor.sprite_pose() {
                Some((action, frame)) => Snapshot::new(format!("Murphy({action:?})"), frame, action.frame_count()),
                None => Snapshot::new("Idle", 0, 1),
            },
            Actor::Bug(Bug::Active(frame)) => Snapshot::new("Bug", frame.index(), 14),
            Actor::Bug(Bug::Dormant(cooldown)) => Snapshot::new("BugDormant", cooldown.elapsed(), cooldown.duration()),
            Actor::Bug(Bug::Held) => Snapshot::new("MurphyPushTarget", 0, 1),
            Actor::Terminal(actor) => Snapshot::new("Terminal", actor.screen_frame(), 7),
            Actor::Explosion(actor) => Snapshot::new(match actor.residue() {
                model::ExplosionResidue::Empty => "Explosion",
                model::ExplosionResidue::Infotron => "ElectronExplosion",
            }, actor.frame().index(), 8),
            Actor::RedDisk(model::RedDisk::Planted(frame)) => Snapshot::new("RedDiskFuse", frame.index(), 40),
            Actor::Base(_) | Actor::YellowDisk(_) | Actor::RedDisk(_) => match self.actor().is_held() {
                true => Snapshot::new("MurphyPushTarget", 0, 1),
                false => Snapshot::new("Idle", 0, 1),
            },
            Actor::RamChip(_) | Actor::Hardware(_) | Actor::Exit(_) | Actor::Port(_) | Actor::InvisibleWall(_) => Snapshot::new("Idle", 0, 1),
        }
    }
}
