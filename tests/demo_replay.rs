//! Semantic replay fingerprints captured before the actor-owned state migration.
//!
//! Hash every cell and session result after every input sample, including sprite
//! timing, momentum, collision occupancy, counters, toggles, and emitted sounds.
//! The fingerprints deliberately exclude Rust struct layout and private enums.

use supaplex_clone::{actors::Actor, demo::Demo, game::Game, level::LevelSet};

mod snapshots {
    use supaplex_clone::actors as model;
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/support/legacy_snapshot.rs"
    ));
}
use snapshots::SnapshotExt;

/// Stable FNV-1a accumulator for explicitly ordered semantic bytes.
struct Fingerprint(u64);

impl Fingerprint {
    /// Starts a fresh history at the standard 64-bit offset basis.
    fn new() -> Self {
        Self(0xcbf29ce484222325)
    }

    /// Incorporates one byte without depending on platform hashing or padding.
    fn byte(&mut self, byte: u8) {
        self.0 = (self.0 ^ u64::from(byte)).wrapping_mul(0x100000001b3);
    }

    /// Encodes an integer in a fixed byte order on every target architecture.
    fn number(&mut self, value: u64) {
        for byte in value.to_le_bytes() {
            self.byte(byte);
        }
    }

    /// Records a named semantic value, separated to avoid ambiguous concatenation.
    fn text(&mut self, value: &str) {
        self.number(value.len() as u64);
        for byte in value.bytes() {
            self.byte(byte);
        }
    }
}

/// Replays every bundled input stream against its pre-refactor semantic history.
#[test]
fn all_original_demo_histories_preserve_actor_and_render_timing() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let bytes = std::fs::read(root.join("assets/data/levels.dat")).unwrap();
    let levels = LevelSet::new(&bytes);
    // Captured from ef691ba before changing actor state storage or behavior.
    // These are parity fixtures, not a claim that every legacy demo solves its
    // level: the baseline already ends some streams while Playing or Dead.
    let expected = [
        0x6561310bb59a571d,
        0x1d35e0c99d83843c,
        0xc112f4f5654a3847,
        0x95fe4a1f8550bdf5,
        0xc45d5592b5e82405,
        0x928210d0e59d031d,
        0x64425c3c8debb7f0,
        0x8d7bd725421971a8,
        0xc65e6cb875ff01ae,
        0x2e09fb1f4ed8faea,
    ];
    for (demo_index, expected) in expected.into_iter().enumerate() {
        let bytes = std::fs::read(root.join(format!("assets/data/demo{demo_index}.bin"))).unwrap();
        let demo = Demo::decode(&bytes, levels.level_count().unwrap()).unwrap();
        let level = levels.load(demo.level_number()).unwrap();
        let mut game = Game::with_random_seed(&level, 0).unwrap();
        let mut observed = Game::with_random_seed(&level, 0).unwrap();
        let mut history = Fingerprint::new();
        // Cache presentation-name hashes so static cells do not allocate on every tick.
        let mut labels = std::collections::HashMap::<String, u64>::new();
        for input in demo.playback() {
            game.tick(input);
            // Recording must observe the same game, not introduce a second
            // simulation algorithm. Keep the original history oracle below.
            let changes = observed.tick_with_changes(input);
            assert_eq!(observed.board(), game.board());
            assert_eq!(observed.tick_count(), game.tick_count());
            assert_eq!(observed.status(), game.status());
            assert_eq!(observed.remaining_infotrons(), game.remaining_infotrons());
            assert_eq!(observed.red_disks(), game.red_disks());
            assert_eq!(observed.gravity(), game.gravity());
            assert_eq!(observed.freeze_zonks(), game.freeze_zonks());
            assert_eq!(observed.freeze_enemies(), game.freeze_enemies());
            assert_eq!(
                observed.terminal_transition_ready(),
                game.terminal_transition_ready()
            );
            assert!(changes.iter().all(|change| change.before != change.after));
            history.number(game.tick_count());
            history.number(u64::from(game.remaining_infotrons()));
            history.number(u64::from(game.red_disks()));
            history.byte(u8::from(game.gravity()));
            history.byte(u8::from(game.freeze_zonks()));
            history.byte(u8::from(game.freeze_enemies()));
            history.byte(u8::from(game.terminal_transition_ready()));
            history.text(&format!("{:?}", game.status()));
            let sounds = game.take_sound_effects();
            assert_eq!(observed.take_sound_effects(), sounds);
            history.text(&format!("{sounds:?}"));
            for cell in game.board().cells() {
                let view = cell.snapshot();
                let kind = match labels.get(view.label()) {
                    Some(hash) => *hash,
                    None => {
                        let mut hash = Fingerprint::new();
                        hash.text(view.label());
                        labels.insert(view.label().to_owned(), hash.0);
                        hash.0
                    }
                };
                history.byte(cell.actor().tile_code());
                history.number(kind);
                history.byte(view.frame());
                history.byte(view.frame_count());
                history.byte(u8::from(cell.is_empty()));
                history.byte(u8::from(cell.is_idle()));
                // Persistent data may affect the next update despite identical artwork.
                match cell.actor() {
                    Actor::Zonk(actor) => history.byte(u8::from(actor.is_falling())),
                    Actor::Infotron(actor) => history.byte(u8::from(actor.is_falling())),
                    Actor::OrangeDisk(actor) => history.byte(u8::from(actor.is_falling())),
                    Actor::Murphy(actor) => history.text(&format!("{:?}", actor.facing())),
                    Actor::SnikSnak(actor) => history.text(&format!("{:?}", actor.heading())),
                    Actor::Electron(actor) => history.text(&format!("{:?}", actor.heading())),
                    Actor::Terminal(actor) => history.byte(u8::from(actor.is_activated())),
                    _ => {}
                }
            }
        }
        assert_eq!(
            history.0, expected,
            "demo {demo_index} diverged from its pre-refactor history"
        );
    }
}
