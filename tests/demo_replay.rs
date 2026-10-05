//! Semantic replay fingerprints after correcting rounded-object callback timing.
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

/// Replays every bundled input stream against the corrected gameplay history.
#[test]
fn all_original_demos_preserve_corrected_actor_timing() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let bytes = std::fs::read(root.join("assets/data/levels.dat")).unwrap();
    let levels = LevelSet::new(&bytes);
    // Rebased after correcting rounded callback timing and the extra terminal
    // quarter tick of Infotron explosions in the last two demos.
    // Independent OpenSupaplex draw/pixel traces cover the corrected scenarios;
    // these histories guard future changes across all bundled demo input ticks.
    let expected = [
        0x98e6c1d2b8c2de6b,
        0x24743fab99cbb03f,
        0x22ef5bedec367ac0,
        0x345a4fe7c887a06c,
        0xc368b60619c22458,
        0x8bff1f8c4752515a,
        0xb0eae4ceda2eae9d,
        0xf48b5bdcf2b4e643,
        0x92b340ba3fc44d6e,
        0x76a8a0e568d5148c,
    ];
    let mut actual = Vec::new();
    for demo_index in 0..expected.len() {
        let bytes = std::fs::read(root.join(format!("assets/data/demo{demo_index}.bin"))).unwrap();
        let demo = Demo::decode(&bytes, levels.level_count().unwrap()).unwrap();
        let level = levels.load(demo.level_number()).unwrap();
        let mut game = Game::with_random_seed(&level, 0).unwrap();
        let mut history = Fingerprint::new();
        // Cache presentation-name hashes so static cells do not allocate on every tick.
        let mut labels = std::collections::HashMap::<String, u64>::new();
        for input in demo.playback() {
            game.tick(input);
            history.number(game.tick_count());
            history.number(u64::from(game.remaining_infotrons()));
            history.number(u64::from(game.red_disks()));
            history.byte(u8::from(game.gravity()));
            history.byte(u8::from(game.freeze_zonks()));
            history.byte(u8::from(game.freeze_enemies()));
            history.byte(u8::from(game.terminal_transition_ready()));
            history.text(&format!("{:?}", game.status()));
            let sounds = game.take_sound_effects();
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
        actual.push(history.0);
    }
    assert!(actual == expected, "demo histories changed: {actual:#x?}");
}
