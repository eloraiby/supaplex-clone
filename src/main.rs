//! Command-line entry point for selecting an original Supaplex level.
//!
//! The SDL2 game loop is added in a later layer; keeping argument and level
//! validation here makes malformed invocations fail before video is started.

use std::process::ExitCode;

use supaplex_clone::{cli::Options, level::LevelSet};

/// Contains the original 111-level set in the executable.
///
/// Embedding the bytes makes `cargo run -- --level N` independent of the
/// caller's current directory while still parsing the supplied DOS data.
const ORIGINAL_LEVELS: &[u8] = include_bytes!("../data/levels.dat");

/// Parses the command line, validates the selected level, and reports errors.
fn main() -> ExitCode {
    // Parse only the arguments after the executable name. `Options` owns all
    // user-facing syntax validation so the eventual SDL front end stays small.
    let options = match Options::parse(std::env::args().skip(1)) {
        Ok(options) => options,
        Err(error) => {
            eprintln!("{error}\n\n{}", Options::usage());
            return ExitCode::FAILURE;
        }
    };

    // Decode the chosen record before initializing any platform resources.
    // This gives corrupt data the same clear error path as bad CLI use.
    let level = match LevelSet::new(ORIGINAL_LEVELS).load(options.level_number()) {
        Ok(level) => level,
        Err(error) => {
            eprintln!("could not load level {}: {error}", options.level_number());
            return ExitCode::FAILURE;
        }
    };

    // The temporary textual handoff proves the complete selection path. The
    // renderer commit replaces this line with the real SDL2 game loop.
    println!("Loaded level {}: {}", options.level_number(), level.title());
    ExitCode::SUCCESS
}
