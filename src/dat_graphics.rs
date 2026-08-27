//! Decoding for the headerless bitmap and palette files shipped with Supaplex.
//!
//! The original DOS game stores its 16-colour pictures as four bitplanes and
//! stores its fonts as one-bit, row-major masks.  Neither form contains image
//! dimensions or a file signature, so callers must supply a known geometry.
//! [`DatAsset`] records the geometries used by the assets needed by the game.
//!
//! `PALETTES.DAT` contains four palettes.  Every colour occupies four bytes:
//! three four-bit VGA colour components followed by an EGA fallback selector.
//! That final byte is deliberately *not* used as PNG alpha; decoded pictures
//! are fully opaque unless a caller explicitly supplies transparent colours to
//! [`decode_binary_rgba`].

use std::{error::Error, fmt, path::Path};

/// Number of independently selectable palettes stored in `PALETTES.DAT`.
pub const PALETTE_COUNT: usize = 4;

/// Number of indexed colours in each original four-bit palette.
pub const COLORS_PER_PALETTE: usize = 16;

/// Number of bytes used by one palette colour record.
const BYTES_PER_PALETTE_ENTRY: usize = 4;

/// Exact byte length of a valid, unmodified `PALETTES.DAT` file.
pub const PALETTES_DAT_SIZE: usize = PALETTE_COUNT * COLORS_PER_PALETTE * BYTES_PER_PALETTE_ENTRY;

/// Width in pixels of the original `FIXED.DAT` sprite strip.
pub const FIXED_WIDTH: u32 = 640;

/// Height in pixels of the original `FIXED.DAT` sprite strip.
pub const FIXED_HEIGHT: u32 = 16;

/// Width in pixels of the original `MOVING.DAT` animation sheet.
pub const MOVING_WIDTH: u32 = 320;

/// Height in pixels of the original `MOVING.DAT` animation sheet.
pub const MOVING_HEIGHT: u32 = 462;

/// Width in pixels of the original `CHARS8.DAT` font bitmap.
pub const CHARS8_WIDTH: u32 = 512;

/// Height in pixels of the original `CHARS8.DAT` font bitmap.
pub const CHARS8_HEIGHT: u32 = 8;

/// Zero-based `PALETTES.DAT` palette used by both gameplay sprite sheets.
pub const GAME_PALETTE_INDEX: usize = 1;

/// Fully opaque RGBA colour used by decoded images and renderer uploads.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Rgba {
    /// Eight-bit red channel.
    pub red: u8,
    /// Eight-bit green channel.
    pub green: u8,
    /// Eight-bit blue channel.
    pub blue: u8,
    /// Eight-bit opacity channel, where zero is transparent.
    pub alpha: u8,
}

impl Rgba {
    /// Opaque black used for zero bits in the default font conversion.
    pub const BLACK: Self = Self::new(0, 0, 0, u8::MAX);

    /// Opaque white used for one bits in the default font conversion.
    pub const WHITE: Self = Self::new(u8::MAX, u8::MAX, u8::MAX, u8::MAX);

    /// Constructs a colour without changing or premultiplying its channels.
    pub const fn new(red: u8, green: u8, blue: u8, alpha: u8) -> Self {
        // Retaining straight (rather than premultiplied) alpha matches both
        // PNG's RGBA representation and SDL2's ordinary texture upload path.
        Self {
            red,
            green,
            blue,
            alpha,
        }
    }

    /// Returns the channels in the byte order required by an RGBA PNG.
    pub const fn to_array(self) -> [u8; 4] {
        // Keeping this conversion in one place prevents the decoder and PNG
        // writer from accidentally disagreeing about channel order.
        [self.red, self.green, self.blue, self.alpha]
    }
}

/// One colour record after separating its VGA colour from its EGA fallback.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PaletteEntry {
    /// Expanded modern RGBA colour derived from the first three nibbles.
    rgba: Rgba,
    /// Original EGA palette-register value stored in the fourth nibble.
    ega_selector: u8,
}

/// Zero-filled placeholder used only while a validated palette is assembled.
const EMPTY_PALETTE_ENTRY: PaletteEntry = PaletteEntry {
    rgba: Rgba::BLACK,
    ega_selector: 0,
};

/// One of the four indexed, 16-colour palettes in `PALETTES.DAT`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Palette {
    /// Fixed-size entries indexed directly by a decoded four-bit pixel.
    entries: [PaletteEntry; COLORS_PER_PALETTE],
}

impl Palette {
    /// Returns the RGBA colour at `index`, or `None` outside `0..16`.
    pub fn rgba(&self, index: usize) -> Option<Rgba> {
        // The optional lookup lets tools inspect untrusted indices without a
        // panic, although valid planar pixels can only produce values 0..15.
        self.entries.get(index).map(|entry| entry.rgba)
    }

    /// Returns the legacy EGA selector at `index`, if the index is valid.
    pub fn ega_selector(&self, index: usize) -> Option<u8> {
        // This value is retained for faithful diagnostics and possible EGA
        // emulation; it must never be confused with an alpha component.
        self.entries.get(index).map(|entry| entry.ega_selector)
    }
}

/// All four palettes decoded from one complete `PALETTES.DAT` byte slice.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Palettes {
    /// Palettes in their original zero-based file order.
    palettes: [Palette; PALETTE_COUNT],
}

impl Palettes {
    /// Parses and validates the exact 256-byte Supaplex palette format.
    pub fn decode(bytes: &[u8]) -> Result<Self, GraphicsError> {
        // A strict size check detects truncated files and also prevents a
        // different raw resource from being mistaken for palette data.
        if bytes.len() != PALETTES_DAT_SIZE {
            return Err(GraphicsError::InvalidPaletteLength {
                expected: PALETTES_DAT_SIZE,
                actual: bytes.len(),
            });
        }

        // Every stored component and EGA selector is a nibble.  Rejecting high
        // bits catches corruption instead of silently discarding information.
        for (offset, value) in bytes.iter().copied().enumerate() {
            if value > 0x0f {
                return Err(GraphicsError::InvalidPaletteNibble { offset, value });
            }
        }

        let empty_palette = Palette {
            entries: [EMPTY_PALETTE_ENTRY; COLORS_PER_PALETTE],
        };
        let mut palettes = [empty_palette; PALETTE_COUNT];

        // Palette records are contiguous, with sixteen four-byte entries in
        // each record.  The first three bytes are R/G/B; byte four targets EGA.
        for (palette_index, palette) in palettes.iter_mut().enumerate() {
            for (color_index, entry) in palette.entries.iter_mut().enumerate() {
                let offset =
                    (palette_index * COLORS_PER_PALETTE + color_index) * BYTES_PER_PALETTE_ENTRY;
                *entry = PaletteEntry {
                    rgba: Rgba::new(
                        expand_nibble(bytes[offset]),
                        expand_nibble(bytes[offset + 1]),
                        expand_nibble(bytes[offset + 2]),
                        u8::MAX,
                    ),
                    ega_selector: bytes[offset + 3],
                };
            }
        }

        Ok(Self { palettes })
    }

    /// Returns a palette by its zero-based file index.
    pub fn get(&self, index: usize) -> Option<&Palette> {
        // Returning a shared reference avoids copying the fixed array when a
        // sprite sheet is decoded or uploaded repeatedly.
        self.palettes.get(index)
    }
}

/// Expands one four-bit VGA component with the historical Supaplex scaling.
fn expand_nibble(value: u8) -> u8 {
    // The DOS loader shifts each stored nibble left twice for the VGA's six-bit
    // DAC.  Shifting left four for an eight-bit channel preserves the same
    // component steps used by established Supaplex graphics converters.
    value << 4
}

/// Headerless encoding used by a known Supaplex bitmap asset.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DatEncoding {
    /// Four one-bit planes per scanline, producing palette indices `0..=15`.
    Planar4Bpp,
    /// One MSB-first bit per pixel, producing a two-colour mask.
    Binary1Bpp,
}

impl fmt::Display for DatEncoding {
    /// Formats a concise encoding name for diagnostics and converter output.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Human-readable labels describe both bit depth and physical layout.
        match self {
            Self::Planar4Bpp => formatter.write_str("planar 4bpp"),
            Self::Binary1Bpp => formatter.write_str("binary 1bpp"),
        }
    }
}

/// Headerless original asset whose dimensions and encoding are known.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DatAsset {
    /// Static 16×16 gameplay tiles arranged in a 640×16 strip.
    Fixed,
    /// Animated gameplay frames arranged in a 320×462 sheet.
    Moving,
    /// Eight-pixel-high binary font glyphs arranged in a 512×8 strip.
    Chars8,
}

impl DatAsset {
    /// Recognizes a converter name such as `fixed` or `FIXED.DAT`.
    pub fn from_name(name: &str) -> Result<Self, GraphicsError> {
        // ASCII case folding matches DOS filenames without imposing Unicode
        // normalization rules on paths supplied by modern callers.
        match name.to_ascii_lowercase().as_str() {
            "fixed" | "fixed.dat" => Ok(Self::Fixed),
            "moving" | "moving.dat" => Ok(Self::Moving),
            "chars8" | "chars8.dat" => Ok(Self::Chars8),
            _ => Err(GraphicsError::UnknownAsset(name.to_owned())),
        }
    }

    /// Recognizes an asset from the final component of a filesystem path.
    pub fn from_path(path: &Path) -> Result<Self, GraphicsError> {
        // Lossy conversion is used only in an error message and for matching
        // ASCII DOS names; no path is reopened from the converted text.
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.to_string_lossy().into_owned());
        Self::from_name(&name)
    }

    /// Returns the canonical lowercase filename of this asset.
    pub const fn file_name(self) -> &'static str {
        // Stable canonical names make CLI help and error messages consistent.
        match self {
            Self::Fixed => "fixed.dat",
            Self::Moving => "moving.dat",
            Self::Chars8 => "chars8.dat",
        }
    }

    /// Returns the width that the raw file itself does not store.
    pub const fn width(self) -> u32 {
        // Each match arm documents the immutable geometry of one DOS asset.
        match self {
            Self::Fixed => FIXED_WIDTH,
            Self::Moving => MOVING_WIDTH,
            Self::Chars8 => CHARS8_WIDTH,
        }
    }

    /// Returns the height that the raw file itself does not store.
    pub const fn height(self) -> u32 {
        // The unusual 462-pixel moving sheet is preserved exactly.
        match self {
            Self::Fixed => FIXED_HEIGHT,
            Self::Moving => MOVING_HEIGHT,
            Self::Chars8 => CHARS8_HEIGHT,
        }
    }

    /// Returns the on-disk pixel encoding used by this asset.
    pub const fn encoding(self) -> DatEncoding {
        // Sprite graphics are planar while the font is a one-bit mask.
        match self {
            Self::Fixed | Self::Moving => DatEncoding::Planar4Bpp,
            Self::Chars8 => DatEncoding::Binary1Bpp,
        }
    }

    /// Returns the zero-based game palette index, or `None` for the font mask.
    pub const fn palette_index(self) -> Option<usize> {
        // Binary font bits use caller-selected monochrome colours and therefore
        // do not consume one of the four palettes stored in `PALETTES.DAT`.
        match self {
            Self::Fixed | Self::Moving => Some(GAME_PALETTE_INDEX),
            Self::Chars8 => None,
        }
    }

    /// Decodes one complete known asset into row-major RGBA bytes.
    pub fn decode(
        self,
        bytes: &[u8],
        palettes: Option<&Palettes>,
    ) -> Result<RgbaImage, GraphicsError> {
        // Known planar assets require palette 1.  The font deliberately uses
        // an opaque black/white mapping so its PNG remains easy to inspect.
        match self {
            Self::Fixed | Self::Moving => {
                let palette_index = self.palette_index().expect("planar asset has a palette");
                let palette = palettes
                    .and_then(|palettes| palettes.get(palette_index))
                    .ok_or(GraphicsError::MissingPalette {
                        asset: self,
                        palette_index,
                    })?;
                decode_planar_rgba(bytes, self.width(), self.height(), palette)
            }
            Self::Chars8 => {
                decode_binary_rgba(bytes, self.width(), self.height(), Rgba::BLACK, Rgba::WHITE)
            }
        }
    }
}

impl fmt::Display for DatAsset {
    /// Formats the canonical DOS-derived filename of an asset.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Delegating to `file_name` guarantees diagnostics use one spelling.
        formatter.write_str(self.file_name())
    }
}

/// Decoded image with tightly packed, top-to-bottom RGBA scanlines.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RgbaImage {
    /// Pixel width of every decoded scanline.
    width: u32,
    /// Number of decoded scanlines.
    height: u32,
    /// Four straight-alpha bytes per pixel in red, green, blue, alpha order.
    pixels: Vec<u8>,
}

impl RgbaImage {
    /// Creates an image after a decoder has validated its byte count.
    fn new(width: u32, height: u32, pixels: Vec<u8>) -> Self {
        // This constructor stays private because the decoder's checked geometry
        // is what guarantees `pixel` can safely calculate byte offsets.
        debug_assert_eq!(
            pixels.len(),
            usize::try_from(width).expect("validated width")
                * usize::try_from(height).expect("validated height")
                * 4
        );
        Self {
            width,
            height,
            pixels,
        }
    }

    /// Returns the image width in pixels.
    pub const fn width(&self) -> u32 {
        // The original value is retained for direct use with `png::Encoder`.
        self.width
    }

    /// Returns the image height in pixels.
    pub const fn height(&self) -> u32 {
        // The original value is retained for direct use with `png::Encoder`.
        self.height
    }

    /// Returns tightly packed bytes suitable for SDL2 or an RGBA PNG writer.
    pub fn as_bytes(&self) -> &[u8] {
        // Borrowing avoids an image-sized allocation during every upload.
        &self.pixels
    }

    /// Returns the colour at `(x, y)`, or `None` when either axis is outside.
    pub fn pixel(&self, x: u32, y: u32) -> Option<Rgba> {
        // Checking axes individually prevents a large x coordinate from
        // wrapping into a later row through row-major index arithmetic.
        if x >= self.width || y >= self.height {
            return None;
        }

        let width = usize::try_from(self.width).expect("validated image width");
        let x = usize::try_from(x).expect("bounded x coordinate");
        let y = usize::try_from(y).expect("bounded y coordinate");
        let offset = (y * width + x) * 4;
        Some(Rgba::new(
            self.pixels[offset],
            self.pixels[offset + 1],
            self.pixels[offset + 2],
            self.pixels[offset + 3],
        ))
    }
}

/// Decodes scanline-interleaved, four-plane Supaplex pixels through `palette`.
pub fn decode_planar_rgba(
    bytes: &[u8],
    width: u32,
    height: u32,
    palette: &Palette,
) -> Result<RgbaImage, GraphicsError> {
    let geometry = validate_geometry(width, height, 4, DatEncoding::Planar4Bpp)?;

    // Four planes contain one half-byte per pixel in total.  Requiring the
    // exact payload size rejects both truncation and unmodelled trailing data.
    if bytes.len() != geometry.encoded_bytes {
        return Err(GraphicsError::InvalidImageLength {
            encoding: DatEncoding::Planar4Bpp,
            width,
            height,
            expected: geometry.encoded_bytes,
            actual: bytes.len(),
        });
    }

    let plane_stride = geometry.width / 8;
    let row_stride = plane_stride * 4;
    let mut pixels = Vec::with_capacity(geometry.rgba_bytes);

    // Each scanline stores all bytes of index bit zero, then bits one, two and
    // three.  Within every plane byte, bit 7 is the leftmost pixel.
    for y in 0..geometry.height {
        let row_offset = y * row_stride;
        for x in 0..geometry.width {
            let source_byte = x / 8;
            let source_mask = 0x80_u8 >> (x % 8);
            let mut palette_index = 0_u8;

            for plane in 0..4 {
                if bytes[row_offset + plane * plane_stride + source_byte] & source_mask != 0 {
                    palette_index |= 1 << plane;
                }
            }

            // A four-plane index is intrinsically within the 16-entry palette,
            // so direct internal indexing is safe after palette construction.
            push_rgba(
                &mut pixels,
                palette.entries[usize::from(palette_index)].rgba,
            );
        }
    }

    Ok(RgbaImage::new(width, height, pixels))
}

/// Decodes a row-major, MSB-first one-bit bitmap into caller-selected colours.
pub fn decode_binary_rgba(
    bytes: &[u8],
    width: u32,
    height: u32,
    zero: Rgba,
    one: Rgba,
) -> Result<RgbaImage, GraphicsError> {
    let geometry = validate_geometry(width, height, 1, DatEncoding::Binary1Bpp)?;

    // Binary scanlines use one byte per eight pixels with no row padding.
    if bytes.len() != geometry.encoded_bytes {
        return Err(GraphicsError::InvalidImageLength {
            encoding: DatEncoding::Binary1Bpp,
            width,
            height,
            expected: geometry.encoded_bytes,
            actual: bytes.len(),
        });
    }

    let row_stride = geometry.width / 8;
    let mut pixels = Vec::with_capacity(geometry.rgba_bytes);

    // Decode in row-major order so the resulting vector can be passed straight
    // to a PNG encoder without transposition or pitch conversion.
    for y in 0..geometry.height {
        for x in 0..geometry.width {
            let source = bytes[y * row_stride + x / 8];
            let mask = 0x80_u8 >> (x % 8);
            push_rgba(&mut pixels, if source & mask == 0 { zero } else { one });
        }
    }

    Ok(RgbaImage::new(width, height, pixels))
}

/// Checked sizes shared by both raw bitmap decoders.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Geometry {
    /// Width converted to the platform's allocation/index type.
    width: usize,
    /// Height converted to the platform's allocation/index type.
    height: usize,
    /// Exact number of bytes expected from the headerless DAT file.
    encoded_bytes: usize,
    /// Exact number of bytes needed by the decoded RGBA vector.
    rgba_bytes: usize,
}

/// Validates byte alignment and performs overflow-safe image size arithmetic.
fn validate_geometry(
    width: u32,
    height: u32,
    bits_per_pixel: usize,
    encoding: DatEncoding,
) -> Result<Geometry, GraphicsError> {
    // Empty images have no meaningful headerless representation and often
    // indicate that a caller forgot to supply the format's external geometry.
    if width == 0 || height == 0 {
        return Err(GraphicsError::ZeroDimension { width, height });
    }

    // Both supported layouts store complete horizontal groups of eight bits;
    // accepting partial bytes would require an unspecified padding convention.
    if !width.is_multiple_of(8) {
        return Err(GraphicsError::WidthNotByteAligned { width, encoding });
    }

    let width_usize =
        usize::try_from(width).map_err(|_| GraphicsError::DimensionsTooLarge { width, height })?;
    let height_usize =
        usize::try_from(height).map_err(|_| GraphicsError::DimensionsTooLarge { width, height })?;
    let pixel_count = width_usize
        .checked_mul(height_usize)
        .ok_or(GraphicsError::DimensionsTooLarge { width, height })?;
    let encoded_bits = pixel_count
        .checked_mul(bits_per_pixel)
        .ok_or(GraphicsError::DimensionsTooLarge { width, height })?;
    let encoded_bytes = encoded_bits / 8;
    let rgba_bytes = pixel_count
        .checked_mul(4)
        .ok_or(GraphicsError::DimensionsTooLarge { width, height })?;

    Ok(Geometry {
        width: width_usize,
        height: height_usize,
        encoded_bytes,
        rgba_bytes,
    })
}

/// Appends one straight-alpha colour in canonical RGBA channel order.
fn push_rgba(destination: &mut Vec<u8>, color: Rgba) {
    // `extend_from_slice` emits exactly four bytes without allocating a
    // temporary vector or relying on a platform-specific integer byte order.
    destination.extend_from_slice(&color.to_array());
}

/// Describes corrupt palette/image input or missing external format metadata.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GraphicsError {
    /// `PALETTES.DAT` did not contain exactly four complete palettes.
    InvalidPaletteLength {
        /// Required size of the complete format.
        expected: usize,
        /// Size of the supplied byte slice.
        actual: usize,
    },
    /// A nominal four-bit palette field contained one or more high bits.
    InvalidPaletteNibble {
        /// Byte offset of the malformed field.
        offset: usize,
        /// Unsupported value read at the offset.
        value: u8,
    },
    /// A raw decoder was asked to create an empty image.
    ZeroDimension {
        /// Supplied pixel width.
        width: u32,
        /// Supplied pixel height.
        height: u32,
    },
    /// A scanline width could not be represented by complete bytes.
    WidthNotByteAligned {
        /// Supplied pixel width.
        width: u32,
        /// Encoding whose scanline alignment was violated.
        encoding: DatEncoding,
    },
    /// Pixel or byte-count arithmetic exceeded the current platform's limits.
    DimensionsTooLarge {
        /// Supplied pixel width.
        width: u32,
        /// Supplied pixel height.
        height: u32,
    },
    /// A raw image did not contain exactly the bytes implied by its geometry.
    InvalidImageLength {
        /// Encoding used to calculate the required length.
        encoding: DatEncoding,
        /// Declared pixel width.
        width: u32,
        /// Declared pixel height.
        height: u32,
        /// Required raw byte count.
        expected: usize,
        /// Supplied raw byte count.
        actual: usize,
    },
    /// A planar known asset was decoded without its required palette set.
    MissingPalette {
        /// Asset that needs an indexed palette.
        asset: DatAsset,
        /// Zero-based palette index required by the asset.
        palette_index: usize,
    },
    /// A filename or converter keyword did not identify a supported asset.
    UnknownAsset(String),
}

impl fmt::Display for GraphicsError {
    /// Formats actionable context for CLI diagnostics and library callers.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Every branch includes both the offending value and the relevant
        // format requirement so conversion failures can be fixed directly.
        match self {
            Self::InvalidPaletteLength { expected, actual } => write!(
                formatter,
                "palette data is {actual} bytes; expected exactly {expected} bytes"
            ),
            Self::InvalidPaletteNibble { offset, value } => write!(
                formatter,
                "palette byte at offset {offset} is {value:#04x}; expected 0x00..=0x0f"
            ),
            Self::ZeroDimension { width, height } => {
                write!(formatter, "image dimensions {width}x{height} are empty")
            }
            Self::WidthNotByteAligned { width, encoding } => write!(
                formatter,
                "{encoding} image width {width} is not divisible by 8"
            ),
            Self::DimensionsTooLarge { width, height } => write!(
                formatter,
                "image dimensions {width}x{height} exceed addressable memory"
            ),
            Self::InvalidImageLength {
                encoding,
                width,
                height,
                expected,
                actual,
            } => write!(
                formatter,
                "{encoding} image {width}x{height} is {actual} bytes; expected exactly {expected} bytes"
            ),
            Self::MissingPalette {
                asset,
                palette_index,
            } => write!(
                formatter,
                "{asset} requires PALETTES.DAT palette {palette_index}"
            ),
            Self::UnknownAsset(name) => write!(
                formatter,
                "unsupported DAT asset {name:?}; expected fixed.dat, moving.dat, or chars8.dat"
            ),
        }
    }
}

impl Error for GraphicsError {}

#[cfg(test)]
mod tests {
    //! Focused format tests plus smoke tests against the bundled DOS assets.

    use super::{
        CHARS8_HEIGHT, CHARS8_WIDTH, COLORS_PER_PALETTE, DatAsset, DatEncoding, FIXED_HEIGHT,
        FIXED_WIDTH, GAME_PALETTE_INDEX, GraphicsError, MOVING_HEIGHT, MOVING_WIDTH,
        PALETTES_DAT_SIZE, Palettes, Rgba, decode_binary_rgba, decode_planar_rgba,
    };

    /// Builds palettes whose first record maps each index to a visible red.
    fn indexed_test_palettes() -> Palettes {
        let mut bytes = vec![0; PALETTES_DAT_SIZE];

        // Distinct red nibbles make the reconstructed palette index directly
        // observable in the decoded RGBA output.
        for index in 0..COLORS_PER_PALETTE {
            bytes[index * 4] = u8::try_from(index).expect("test index is a nibble");
            bytes[index * 4 + 3] = u8::try_from(index).expect("test index is a nibble");
        }

        Palettes::decode(&bytes).expect("synthetic palette should be valid")
    }

    /// Confirms RGB nibble expansion and preservation of the non-alpha byte.
    #[test]
    fn decodes_palette_components_and_ega_selector() {
        let mut bytes = vec![0; PALETTES_DAT_SIZE];
        bytes[0..4].copy_from_slice(&[0x0, 0x8, 0xf, 0x4]);

        let palettes = Palettes::decode(&bytes).expect("palette should decode");
        let first = palettes.get(0).expect("palette zero should exist");

        assert_eq!(first.rgba(0), Some(Rgba::new(0x00, 0x80, 0xf0, 0xff)));
        assert_eq!(first.ega_selector(0), Some(0x4));
        assert_eq!(first.rgba(COLORS_PER_PALETTE), None);
    }

    /// Confirms malformed palette sizes and high bits are reported precisely.
    #[test]
    fn rejects_malformed_palette_data() {
        assert_eq!(
            Palettes::decode(&[0; 3]),
            Err(GraphicsError::InvalidPaletteLength {
                expected: PALETTES_DAT_SIZE,
                actual: 3,
            })
        );

        let mut bytes = vec![0; PALETTES_DAT_SIZE];
        bytes[73] = 0x10;
        assert_eq!(
            Palettes::decode(&bytes),
            Err(GraphicsError::InvalidPaletteNibble {
                offset: 73,
                value: 0x10,
            })
        );
    }

    /// Confirms plane order and MSB-first horizontal order with all index bits.
    #[test]
    fn decodes_interleaved_planar_scanlines() {
        let palettes = indexed_test_palettes();
        let palette = palettes.get(0).expect("palette zero should exist");
        let bytes = [0b1000_0001, 0b0100_0010, 0b0010_0100, 0b0001_1000];

        let image = decode_planar_rgba(&bytes, 8, 1, palette).expect("image should decode");
        let expected_indices = [1_u8, 2, 4, 8, 8, 4, 2, 1];

        for (x, index) in expected_indices.into_iter().enumerate() {
            assert_eq!(
                image.pixel(u32::try_from(x).expect("small x"), 0),
                Some(Rgba::new(index * 16, 0, 0, u8::MAX))
            );
        }
    }

    /// Confirms binary bytes decode left-to-right from their most significant bit.
    #[test]
    fn decodes_binary_scanlines_with_custom_colours() {
        let zero = Rgba::new(1, 2, 3, 4);
        let one = Rgba::new(5, 6, 7, 8);
        let image =
            decode_binary_rgba(&[0b1010_0001], 8, 1, zero, one).expect("binary row should decode");

        assert_eq!(image.pixel(0, 0), Some(one));
        assert_eq!(image.pixel(1, 0), Some(zero));
        assert_eq!(image.pixel(2, 0), Some(one));
        assert_eq!(image.pixel(7, 0), Some(one));
        assert_eq!(image.pixel(8, 0), None);
    }

    /// Confirms geometry validation happens before any unsafe indexing occurs.
    #[test]
    fn rejects_unaligned_and_incorrectly_sized_images() {
        let palette = indexed_test_palettes();
        assert_eq!(
            decode_planar_rgba(&[], 7, 1, palette.get(0).expect("palette zero")),
            Err(GraphicsError::WidthNotByteAligned {
                width: 7,
                encoding: DatEncoding::Planar4Bpp,
            })
        );
        assert_eq!(
            decode_binary_rgba(&[], 8, 1, Rgba::BLACK, Rgba::WHITE),
            Err(GraphicsError::InvalidImageLength {
                encoding: DatEncoding::Binary1Bpp,
                width: 8,
                height: 1,
                expected: 1,
                actual: 0,
            })
        );
    }

    /// Confirms DOS-style names are recognized case-insensitively.
    #[test]
    fn recognizes_supported_asset_names() {
        assert_eq!(DatAsset::from_name("FIXED.DAT"), Ok(DatAsset::Fixed));
        assert_eq!(DatAsset::from_name("moving"), Ok(DatAsset::Moving));
        assert_eq!(DatAsset::from_name("Chars8.dat"), Ok(DatAsset::Chars8));
        assert!(matches!(
            DatAsset::from_name("panel.dat"),
            Err(GraphicsError::UnknownAsset(_))
        ));
    }

    /// Decodes all three bundled files to verify their documented geometries.
    #[test]
    fn decodes_bundled_gameplay_assets() {
        let palettes = Palettes::decode(include_bytes!("../data/palettes.dat"))
            .expect("bundled palettes should decode");
        let cases = [
            (
                DatAsset::Fixed,
                include_bytes!("../data/fixed.dat").as_slice(),
            ),
            (
                DatAsset::Moving,
                include_bytes!("../data/moving.dat").as_slice(),
            ),
            (
                DatAsset::Chars8,
                include_bytes!("../data/chars8.dat").as_slice(),
            ),
        ];

        for (asset, bytes) in cases {
            let image = asset
                .decode(bytes, Some(&palettes))
                .expect("bundled asset should decode");
            assert_eq!(image.width(), asset.width());
            assert_eq!(image.height(), asset.height());
            assert_eq!(
                image.as_bytes().len(),
                usize::try_from(asset.width()).expect("known width")
                    * usize::try_from(asset.height()).expect("known height")
                    * 4
            );
        }

        assert_eq!((FIXED_WIDTH, FIXED_HEIGHT), (640, 16));
        assert_eq!((MOVING_WIDTH, MOVING_HEIGHT), (320, 462));
        assert_eq!((CHARS8_WIDTH, CHARS8_HEIGHT), (512, 8));
        assert_eq!(DatAsset::Fixed.palette_index(), Some(GAME_PALETTE_INDEX));
    }
}
