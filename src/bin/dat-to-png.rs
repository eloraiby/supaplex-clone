//! Converts the headerless Supaplex gameplay graphics into ordinary RGBA PNGs.
//!
//! The input filename selects one of the geometries known by
//! [`DatAsset`].  `FIXED.DAT` and
//! `MOVING.DAT` use palette 1 from `PALETTES.DAT`; if `--palettes` is omitted,
//! the converter looks for lowercase `palettes.dat` beside the input image.
//! `CHARS8.DAT` is converted as an opaque black-and-white font mask.

use std::{
    error::Error,
    ffi::OsString,
    fmt,
    fs::File,
    io::{self, BufWriter},
    path::{Path, PathBuf},
    process::ExitCode,
};

use supaplex_clone::dat_graphics::{DatAsset, GraphicsError, Palettes, RgbaImage};

/// Complete command syntax and examples shown for help and argument failures.
const USAGE: &str = "Usage: dat-to-png <INPUT.DAT> <OUTPUT.PNG> [--palettes <PALETTES.DAT>]

Supported input basenames and their raw formats:
  fixed.dat   640x16,  planar 4bpp, PALETTES.DAT palette 1
  moving.dat  320x462, planar 4bpp, PALETTES.DAT palette 1
  chars8.dat  512x8,   binary 1bpp, opaque black and white

When --palettes is omitted for a planar image, palettes.dat is read from the
input file's directory.

Examples:
  dat-to-png data/fixed.dat fixed.png
  dat-to-png data/moving.dat moving.png --palettes data/palettes.dat
  dat-to-png data/chars8.dat chars8.png";

/// Files selected by one validated converter invocation.
#[derive(Clone, Debug, Eq, PartialEq)]
struct Options {
    /// Raw DAT bitmap whose basename also determines its geometry.
    input: PathBuf,
    /// PNG file created or replaced by a successful conversion.
    output: PathBuf,
    /// Explicit palette file, or `None` to use the input's sibling file.
    palettes: Option<PathBuf>,
}

/// Result of parsing arguments that may request help instead of conversion.
#[derive(Clone, Debug, Eq, PartialEq)]
enum ParseOutcome {
    /// Print [`USAGE`] and exit successfully without reading any files.
    Help,
    /// Run a conversion with the contained validated positional arguments.
    Convert(Options),
}

/// Parses the deliberately small command grammar without a CLI dependency.
fn parse_arguments<I>(arguments: I) -> Result<ParseOutcome, CliError>
where
    I: IntoIterator<Item = OsString>,
{
    // Materializing the arguments makes the accepted two- or four-token forms
    // explicit and preserves non-Unicode input/output paths through `OsString`.
    let arguments: Vec<OsString> = arguments.into_iter().collect();

    if arguments.len() == 1 && (arguments[0] == "--help" || arguments[0] == "-h") {
        return Ok(ParseOutcome::Help);
    }

    if arguments.len() != 2 && arguments.len() != 4 {
        return Err(CliError::InvalidArguments(
            "expected an input DAT path, an output PNG path, and optionally --palettes <path>"
                .to_owned(),
        ));
    }

    let input = PathBuf::from(&arguments[0]);
    let output = PathBuf::from(&arguments[1]);
    let palettes = if arguments.len() == 4 {
        if arguments[2] != "--palettes" {
            return Err(CliError::InvalidArguments(format!(
                "unexpected option {:?}; expected --palettes",
                arguments[2]
            )));
        }

        Some(PathBuf::from(&arguments[3]))
    } else {
        None
    };

    Ok(ParseOutcome::Convert(Options {
        input,
        output,
        palettes,
    }))
}

/// Runs one conversion and reports its resolved format after writing the PNG.
fn convert(options: &Options) -> Result<(), CliError> {
    // The raw file has no header, so its DOS basename is the only available
    // source of dimensions and encoding in this intentionally narrow tool.
    let asset = DatAsset::from_path(&options.input).map_err(CliError::Graphics)?;
    let input_bytes = read_file(&options.input, "read input bitmap")?;

    // Only planar sprite sheets need PALETTES.DAT.  Avoiding the palette read
    // for `CHARS8.DAT` lets the standalone font conversion remain self-contained.
    let palettes = if asset.palette_index().is_some() {
        let palette_path = options
            .palettes
            .clone()
            .unwrap_or_else(|| default_palette_path(&options.input));
        let palette_bytes = read_file(&palette_path, "read palette data")?;
        Some(Palettes::decode(&palette_bytes).map_err(CliError::Graphics)?)
    } else {
        None
    };

    let image = asset
        .decode(&input_bytes, palettes.as_ref())
        .map_err(CliError::Graphics)?;
    write_png(&options.output, &image)?;

    // A concise success line records all external geometry that was absent from
    // the DAT payload and is useful in build scripts which convert several files.
    println!(
        "wrote {}x{} {} image to {}",
        image.width(),
        image.height(),
        asset.encoding(),
        options.output.display()
    );
    Ok(())
}

/// Selects lowercase `palettes.dat` in the raw image's containing directory.
fn default_palette_path(input: &Path) -> PathBuf {
    // `with_file_name` also handles a bare `fixed.dat` by producing a sibling
    // path in the current directory, without depending on the process CWD here.
    input.with_file_name("palettes.dat")
}

/// Reads an entire small Supaplex resource and attaches its path to I/O errors.
fn read_file(path: &Path, action: &'static str) -> Result<Vec<u8>, CliError> {
    // `std::fs::read` is appropriate for the largest supported input (73,920
    // bytes) and lets the format decoder validate the complete payload at once.
    std::fs::read(path).map_err(|source| CliError::Io {
        action,
        path: path.to_owned(),
        source,
    })
}

/// Creates an RGBA8 PNG and writes the decoded scanlines without transformation.
fn write_png(path: &Path, image: &RgbaImage) -> Result<(), CliError> {
    // File creation is kept separate from PNG encoding so permission and parent
    // directory failures retain an ordinary, actionable operating-system error.
    let file = File::create(path).map_err(|source| CliError::Io {
        action: "create output PNG",
        path: path.to_owned(),
        source,
    })?;
    let output = BufWriter::new(file);

    // The module already emits tightly packed straight-alpha RGBA bytes, so no
    // palette chunk, stride adjustment, or per-pixel conversion is necessary.
    let mut encoder = png::Encoder::new(output, image.width(), image.height());
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().map_err(|source| CliError::Png {
        path: path.to_owned(),
        source,
    })?;
    writer
        .write_image_data(image.as_bytes())
        .map_err(|source| CliError::Png {
            path: path.to_owned(),
            source,
        })?;

    Ok(())
}

/// Failures from command parsing, resource I/O, DAT decoding, or PNG encoding.
#[derive(Debug)]
enum CliError {
    /// Command-line tokens did not match either accepted invocation form.
    InvalidArguments(String),
    /// A filesystem operation failed before or while preparing conversion data.
    Io {
        /// Short present-tense description of the failed operation.
        action: &'static str,
        /// Exact user-supplied or inferred path involved in the operation.
        path: PathBuf,
        /// Original operating-system error retained for its kind and source.
        source: io::Error,
    },
    /// Raw DAT bytes or an inferred asset name violated the decoded format.
    Graphics(GraphicsError),
    /// The PNG crate could not write a valid header or image-data stream.
    Png {
        /// Destination path receiving the PNG stream.
        path: PathBuf,
        /// Original encoder error retained for detailed diagnostics.
        source: png::EncodingError,
    },
}

impl fmt::Display for CliError {
    /// Formats one-line diagnostics suitable for direct terminal display.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Paths are formatted with `display` so non-Unicode paths remain usable
        // on platforms where they are valid filesystem names.
        match self {
            Self::InvalidArguments(message) => formatter.write_str(message),
            Self::Io {
                action,
                path,
                source,
            } => write!(formatter, "could not {action} {}: {source}", path.display()),
            Self::Graphics(source) => source.fmt(formatter),
            Self::Png { path, source } => {
                write!(formatter, "could not encode {}: {source}", path.display())
            }
        }
    }
}

impl Error for CliError {
    /// Exposes wrapped errors to callers that print a diagnostic source chain.
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        // Pure argument errors have no lower-level cause; all remaining variants
        // retain their original error rather than flattening it into text.
        match self {
            Self::InvalidArguments(_) => None,
            Self::Io { source, .. } => Some(source),
            Self::Graphics(source) => Some(source),
            Self::Png { source, .. } => Some(source),
        }
    }
}

/// Parses process arguments, performs conversion, and selects a shell exit code.
fn main() -> ExitCode {
    // Only arguments after the executable name participate in the documented
    // grammar; the binary path varies between Cargo and installed invocations.
    match parse_arguments(std::env::args_os().skip(1)) {
        Ok(ParseOutcome::Help) => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        Ok(ParseOutcome::Convert(options)) => match convert(&options) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("error: {error}");
                ExitCode::FAILURE
            }
        },
        Err(error) => {
            eprintln!("error: {error}\n\n{USAGE}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    //! Tests for help, default paths, and both accepted argument forms.

    use std::{ffi::OsString, path::PathBuf};

    use super::{CliError, Options, ParseOutcome, default_palette_path, parse_arguments};

    /// Converts string literals to the platform-native argument type.
    fn args(values: &[&str]) -> Vec<OsString> {
        // Tests use ASCII tokens while the production parser remains capable
        // of preserving arbitrary platform paths through `OsString`.
        values.iter().map(OsString::from).collect()
    }

    /// Confirms both conventional help spellings bypass file conversion.
    #[test]
    fn parses_help_flags() {
        assert_eq!(
            parse_arguments(args(&["--help"])).expect("long help should parse"),
            ParseOutcome::Help
        );
        assert_eq!(
            parse_arguments(args(&["-h"])).expect("short help should parse"),
            ParseOutcome::Help
        );
    }

    /// Confirms the compact form records input/output and defers palette choice.
    #[test]
    fn parses_default_palette_form() {
        let parsed = parse_arguments(args(&["data/fixed.dat", "fixed.png"]))
            .expect("compact invocation should parse");

        assert_eq!(
            parsed,
            ParseOutcome::Convert(Options {
                input: PathBuf::from("data/fixed.dat"),
                output: PathBuf::from("fixed.png"),
                palettes: None,
            })
        );
    }

    /// Confirms an explicit palette path is retained without normalization.
    #[test]
    fn parses_explicit_palette_form() {
        let parsed = parse_arguments(args(&[
            "data/moving.dat",
            "moving.png",
            "--palettes",
            "custom/PALETTES.DAT",
        ]))
        .expect("explicit invocation should parse");

        assert_eq!(
            parsed,
            ParseOutcome::Convert(Options {
                input: PathBuf::from("data/moving.dat"),
                output: PathBuf::from("moving.png"),
                palettes: Some(PathBuf::from("custom/PALETTES.DAT")),
            })
        );
    }

    /// Confirms unknown options and missing positionals produce usage errors.
    #[test]
    fn rejects_invalid_argument_shapes() {
        assert!(matches!(
            parse_arguments(args(&["data/fixed.dat"])),
            Err(CliError::InvalidArguments(_))
        ));
        assert!(matches!(
            parse_arguments(args(&[
                "data/fixed.dat",
                "fixed.png",
                "--palette",
                "data/palettes.dat",
            ])),
            Err(CliError::InvalidArguments(_))
        ));
    }

    /// Confirms the inferred palette remains beside nested and bare inputs.
    #[test]
    fn infers_sibling_palette_path() {
        assert_eq!(
            default_palette_path(PathBuf::from("data/fixed.dat").as_path()),
            PathBuf::from("data/palettes.dat")
        );
        assert_eq!(
            default_palette_path(PathBuf::from("moving.dat").as_path()),
            PathBuf::from("palettes.dat")
        );
    }
}
