//! Semantic replay fingerprints after correcting rounded movement source ownership.
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

/// Replays every bundled input stream against the corrected occupancy history.
#[test]
fn all_demo_histories_preserve_corrected_movement_ownership() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let bytes = std::fs::read(root.join("assets/data/levels.dat")).unwrap();
    let levels = LevelSet::new(&bytes);
    // The earlier ef691ba fixtures preserved a bug: rolls and roll-to-fall
    // transfers exposed their source immediately. The targeted source-lifetime
    // and mirrored pixel regressions establish the correction independently;
    // these hashes then detect unrelated changes across all supplied demos.
    // They do not claim upstream equivalence or successful completion of every demo.
    let expected = [
        0xc51b78d5c0d0e0f0,
        0x8d51a138735b25f1,
        0x18456193044496e5,
        0xdefec9be17950b9b,
        0xdc9a6106c7305b11,
        0x08b01ce7cd141553,
        0xb119616a9f1c2744,
        0x18f1d3eb11fd1e84,
        0xfaa7f8c56fbd536d,
        0xaa6d701dac3b22c6,
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
            history.text(&format!("{:?}", game.take_sound_effects()));
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
    assert_eq!(actual, expected, "demo histories changed");
}
