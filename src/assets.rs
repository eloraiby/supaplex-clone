//! Compile-time or filesystem-backed access to production game assets.
//!
//! Normal builds retain the self-contained executable behavior by borrowing
//! bytes emitted by `include_bytes!`. Enabling the `unbundle` Cargo feature
//! removes those payloads from production code and reads the identical paths
//! from disk. `SUPAPLEX_ASSET_ROOT` may name the directory containing `data/`
//! and `assets/`; without it, relative paths resolve from the working directory.

use std::{error::Error, fmt};

#[cfg(feature = "unbundle")]
use std::{env, fs, io, path::PathBuf};

/// Environment variable used to relocate an unbundled production asset tree.
pub const ASSET_ROOT_ENVIRONMENT_VARIABLE: &str = "SUPAPLEX_ASSET_ROOT";

/// Relative location of the original level collection.
#[cfg(feature = "unbundle")]
const LEVELS_PATH: &str = "data/levels.dat";

/// Relative location of the original tracker arrangement.
#[cfg(feature = "unbundle")]
const MUSIC_PATH: &str = "assets/audio/music.xm";

/// Relative Sound Blaster effect locations in semantic sound-effect order.
#[cfg(feature = "unbundle")]
const EFFECT_PATHS: [&str; 7] = [
    "assets/audio/explosion.wav",
    "assets/audio/infotron.wav",
    "assets/audio/push.wav",
    "assets/audio/fall.wav",
    "assets/audio/bug.wav",
    "assets/audio/base.wav",
    "assets/audio/exit.wav",
];

/// Relative location of the converted fixed-tile strip.
pub(crate) const FIXED_GRAPHICS_PATH: &str = "assets/gfx/fixed.png";

/// Relative location of the converted moving-actor sheet.
pub(crate) const MOVING_GRAPHICS_PATH: &str = "assets/gfx/moving.png";

/// Relative location of the converted eight-pixel font.
pub(crate) const FONT_GRAPHICS_PATH: &str = "assets/gfx/chars8.png";

/// Relative location of the converted six-pixel-advance menu font.
pub(crate) const MENU_FONT_GRAPHICS_PATH: &str = "assets/gfx/chars6.png";

/// Relative location of the original title screen conversion.
pub(crate) const TITLE_GRAPHICS_PATH: &str = "assets/gfx/title.png";

/// Relative location of the original main-menu background conversion.
pub(crate) const MENU_GRAPHICS_PATH: &str = "assets/gfx/menu.png";

/// Relative location of the original in-game status-panel conversion.
pub(crate) const PANEL_GRAPHICS_PATH: &str = "assets/gfx/panel.png";

/// Bytes borrowed from the executable or owned after an unbundled file read.
#[derive(Debug)]
pub struct AssetBytes {
    /// Feature-selected representation hidden behind a uniform byte-slice API.
    storage: AssetStorage,
}

/// Internal ownership selected at compile time for one production payload.
#[derive(Debug)]
enum AssetStorage {
    /// Static data emitted into an ordinary self-contained executable.
    #[cfg(not(feature = "unbundle"))]
    Embedded(&'static [u8]),
    /// Heap-owned data read from the external asset tree during initialization.
    #[cfg(feature = "unbundle")]
    External(Box<[u8]>),
}

impl AssetBytes {
    /// Wraps bytes already stored in an ordinary bundled executable.
    #[cfg(not(feature = "unbundle"))]
    const fn embedded(bytes: &'static [u8]) -> Self {
        // Keeping the static slice borrowed avoids copying large audio and level
        // payloads merely to pass them through startup decoders.
        Self {
            storage: AssetStorage::Embedded(bytes),
        }
    }

    /// Wraps bytes read from an unbundled filesystem asset.
    #[cfg(feature = "unbundle")]
    fn external(bytes: Vec<u8>) -> Self {
        // A boxed slice retains exactly the file payload with no spare vector
        // capacity while its consumer parses or uploads the data.
        Self {
            storage: AssetStorage::External(bytes.into_boxed_slice()),
        }
    }
}

impl AsRef<[u8]> for AssetBytes {
    /// Borrows the payload without exposing whether it came from disk or `.rodata`.
    fn as_ref(&self) -> &[u8] {
        // Each feature compiles exactly one storage variant, so this match has
        // no runtime policy choice and optimizes to a direct slice projection.
        match &self.storage {
            #[cfg(not(feature = "unbundle"))]
            AssetStorage::Embedded(bytes) => bytes,
            #[cfg(feature = "unbundle")]
            AssetStorage::External(bytes) => bytes,
        }
    }
}

/// Complete graphics payload set loaded before SDL texture creation.
#[derive(Debug)]
pub(crate) struct GraphicsAssets {
    /// Original fixed-tile strip converted to an RGBA PNG.
    pub(crate) fixed: AssetBytes,
    /// Original moving-actor sheet converted to an RGBA PNG.
    pub(crate) moving: AssetBytes,
    /// Original eight-pixel font converted to an RGBA PNG.
    pub(crate) font: AssetBytes,
    /// Original six-pixel-advance menu font converted to an RGBA PNG.
    pub(crate) menu_font: AssetBytes,
    /// Original title artwork converted with its executable-resident palette.
    pub(crate) title: AssetBytes,
    /// Original main-menu background converted with gameplay palette 1.
    pub(crate) menu: AssetBytes,
    /// Original bottom status-panel artwork converted with gameplay palette 1.
    pub(crate) panel: AssetBytes,
}

/// Complete soundtrack and effect payload set loaded before opening SDL audio.
#[derive(Debug)]
pub(crate) struct AudioAssets {
    /// Original AdLib arrangement retained as a FastTracker XM module.
    pub(crate) music: AssetBytes,
    /// Sound Blaster WAV renders in the stable semantic effect order.
    pub(crate) effects: [AssetBytes; 7],
}

/// Loads the original level set from the selected bundled or external source.
pub fn load_levels() -> Result<AssetBytes, AssetError> {
    // Keep the compile-time branches inside the access layer so gameplay never
    // needs separate lifetime or ownership logic for the two distribution modes.
    #[cfg(not(feature = "unbundle"))]
    {
        Ok(AssetBytes::embedded(include_bytes!("../data/levels.dat")))
    }
    #[cfg(feature = "unbundle")]
    {
        load_external(LEVELS_PATH)
    }
}

/// Loads all graphics atlases from the selected bundled or external source.
pub(crate) fn load_graphics() -> Result<GraphicsAssets, AssetError> {
    // Loading the complete set first makes missing unbundled files fail before
    // SDL receives a partial texture collection.
    #[cfg(not(feature = "unbundle"))]
    {
        Ok(GraphicsAssets {
            fixed: AssetBytes::embedded(include_bytes!("../assets/gfx/fixed.png")),
            moving: AssetBytes::embedded(include_bytes!("../assets/gfx/moving.png")),
            font: AssetBytes::embedded(include_bytes!("../assets/gfx/chars8.png")),
            menu_font: AssetBytes::embedded(include_bytes!("../assets/gfx/chars6.png")),
            title: AssetBytes::embedded(include_bytes!("../assets/gfx/title.png")),
            menu: AssetBytes::embedded(include_bytes!("../assets/gfx/menu.png")),
            panel: AssetBytes::embedded(include_bytes!("../assets/gfx/panel.png")),
        })
    }
    #[cfg(feature = "unbundle")]
    {
        Ok(GraphicsAssets {
            fixed: load_external(FIXED_GRAPHICS_PATH)?,
            moving: load_external(MOVING_GRAPHICS_PATH)?,
            font: load_external(FONT_GRAPHICS_PATH)?,
            menu_font: load_external(MENU_FONT_GRAPHICS_PATH)?,
            title: load_external(TITLE_GRAPHICS_PATH)?,
            menu: load_external(MENU_GRAPHICS_PATH)?,
            panel: load_external(PANEL_GRAPHICS_PATH)?,
        })
    }
}

/// Loads music and all seven effects from the selected production source.
pub(crate) fn load_audio() -> Result<AudioAssets, AssetError> {
    // The fixed array preserves the contract with `SoundEffect::index`; a
    // missing external clip fails initialization instead of shifting later clips.
    #[cfg(not(feature = "unbundle"))]
    {
        Ok(AudioAssets {
            music: AssetBytes::embedded(include_bytes!("../assets/audio/music.xm")),
            effects: [
                AssetBytes::embedded(include_bytes!("../assets/audio/explosion.wav")),
                AssetBytes::embedded(include_bytes!("../assets/audio/infotron.wav")),
                AssetBytes::embedded(include_bytes!("../assets/audio/push.wav")),
                AssetBytes::embedded(include_bytes!("../assets/audio/fall.wav")),
                AssetBytes::embedded(include_bytes!("../assets/audio/bug.wav")),
                AssetBytes::embedded(include_bytes!("../assets/audio/base.wav")),
                AssetBytes::embedded(include_bytes!("../assets/audio/exit.wav")),
            ],
        })
    }
    #[cfg(feature = "unbundle")]
    {
        Ok(AudioAssets {
            music: load_external(MUSIC_PATH)?,
            effects: [
                load_external(EFFECT_PATHS[0])?,
                load_external(EFFECT_PATHS[1])?,
                load_external(EFFECT_PATHS[2])?,
                load_external(EFFECT_PATHS[3])?,
                load_external(EFFECT_PATHS[4])?,
                load_external(EFFECT_PATHS[5])?,
                load_external(EFFECT_PATHS[6])?,
            ],
        })
    }
}

/// Resolves and reads one file from an unbundled asset tree.
#[cfg(feature = "unbundle")]
fn load_external(relative_path: &'static str) -> Result<AssetBytes, AssetError> {
    // An explicit root supports launching a packaged binary from any working
    // directory. Omitting it deliberately preserves convenient project-root use.
    let path = match env::var_os(ASSET_ROOT_ENVIRONMENT_VARIABLE) {
        Some(root) => PathBuf::from(root).join(relative_path),
        None => PathBuf::from(relative_path),
    };
    fs::read(&path)
        .map(AssetBytes::external)
        .map_err(|source| AssetError { path, source })
}

/// Failure to read one required file in an `unbundle` build.
#[cfg(feature = "unbundle")]
#[derive(Debug)]
pub struct AssetError {
    /// Fully resolved path attempted by the loader.
    path: PathBuf,
    /// Operating-system error returned by the filesystem read.
    source: io::Error,
}

/// Uninhabited asset-loading error retained by the bundled API signature.
#[cfg(not(feature = "unbundle"))]
#[derive(Debug)]
pub struct AssetError {
    /// Prevents construction while allowing one shared fallible public API.
    never: std::convert::Infallible,
}

impl fmt::Display for AssetError {
    /// Formats the failed path and its operating-system reason when unbundled.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Bundled loading is infallible; its branch is statically unreachable.
        #[cfg(feature = "unbundle")]
        {
            write!(
                formatter,
                "could not read unbundled asset {}: {}",
                self.path.display(),
                self.source
            )
        }
        #[cfg(not(feature = "unbundle"))]
        {
            let _ = formatter;
            match self.never {}
        }
    }
}

impl Error for AssetError {
    /// Exposes the underlying filesystem failure in an `unbundle` build.
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        // There is no bundled error value; external reads retain their exact
        // `io::Error` so callers and diagnostics can inspect its error kind.
        #[cfg(feature = "unbundle")]
        {
            Some(&self.source)
        }
        #[cfg(not(feature = "unbundle"))]
        {
            match self.never {}
        }
    }
}

#[cfg(test)]
mod tests {
    //! Asset-set checks shared by bundled and unbundled feature configurations.

    use super::{load_audio, load_graphics, load_levels};

    /// Confirms every production payload is available through the selected mode.
    #[test]
    fn production_asset_set_loads_with_expected_signatures() {
        let levels = load_levels().expect("production levels should load");
        let graphics = load_graphics().expect("production graphics should load");
        let audio = load_audio().expect("production audio should load");

        // Exact sizes and lightweight signatures detect misplaced files without
        // duplicating the format-specific validation performed by their consumers.
        assert_eq!(levels.as_ref().len(), 170_496);
        for png in [
            &graphics.fixed,
            &graphics.moving,
            &graphics.font,
            &graphics.menu_font,
            &graphics.title,
            &graphics.menu,
            &graphics.panel,
        ] {
            assert!(png.as_ref().starts_with(b"\x89PNG\r\n\x1a\n"));
        }
        assert!(audio.music.as_ref().starts_with(b"Extended Module: "));
        assert_eq!(audio.effects.len(), 7);
        assert!(
            audio
                .effects
                .iter()
                .all(|effect| effect.as_ref().starts_with(b"RIFF"))
        );
    }
}
