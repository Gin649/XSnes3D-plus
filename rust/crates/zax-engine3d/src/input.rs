//! The engine's input: the tile records and frame batch it learns from, and the widescreen
//! limits. `XSnes3D+`: copied from Xnes3D+ v1.0.0 (`zax-nes/src/tile_record.rs`,
//! `frame_batch.rs` and the constants of `wide.rs`), so the engine no longer depends on the
//! NES core. Later steps change these for the SNES.

/// One 8x8 tile displayed during a frame (REQ-CORE-034, REQ-CORE-035, SPEC-02 §4.1).
///
/// The two bit planes hold the 16 CHR bytes of the pattern *without flips applied*. The byte
/// of row `r` (0 = top) occupies bits `8r` to `8r+7`; inside that byte bit 7 is the leftmost
/// pixel (column 0) and bit 0 is column 7. The colour of the source pixel at (column c,
/// row r) is `bit(plane0, 8r + 7 - c) + 2 * bit(plane1, 8r + 7 - c)`, between 0 and 3, where
/// 0 is transparent. The pixel shown at tile position (px, py) is the source pixel of column
/// `mirror_h ? 7 - px : px` and row `mirror_v ? 7 - py : py`.
///
/// This encoding is also the one used by the pattern planes stored in profiles (SPEC-03),
/// which are the recognition key of the conversion engine (SPEC-04); it must be respected to
/// the bit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[allow(clippy::struct_excessive_bools)] // The record of SPEC-02 §4.1 has four flags.
pub struct TileRecord {
    /// Low bit plane, 8 CHR bytes packed little-endian by row.
    pub plane0: u64,
    /// High bit plane, 8 CHR bytes packed little-endian by row.
    pub plane1: u64,
    /// Screen x of the tile's top-left pixel. Background: -7..255. Sprite: 0..255.
    pub x: i16,
    /// Screen y of the tile's top-left pixel. Background: -1..238. Sprite: 1..239.
    pub y: i16,
    /// Background: REQ-CORE-040. Sprite: 0..127 (REQ-CORE-043).
    pub index: i32,
    /// Horizontal flip. Sprite: bit 6 of the OAM attribute. Background: false.
    pub mirror_h: bool,
    /// Vertical flip. Sprite: bit 7 of the OAM attribute. Background: false.
    pub mirror_v: bool,
    /// Sprite priority bit "behind background" (bit 5 of the attribute). Always false for
    /// background.
    pub hidden: bool,
    /// Sub-palette number, 0 to 3; background sub-palette for background tiles.
    pub palette: u8,
    /// True for a background tile, false for a sprite tile.
    pub background: bool,
}

impl TileRecord {
    /// Creates a tile record.
    #[must_use]
    #[allow(clippy::too_many_arguments)] // The ten fields of SPEC-02 §4.1, in their order.
    #[allow(clippy::fn_params_excessive_bools)] // Same.
    pub const fn new(
        plane0: u64,
        plane1: u64,
        x: i16,
        y: i16,
        index: i32,
        mirror_h: bool,
        mirror_v: bool,
        hidden: bool,
        palette: u8,
        background: bool,
    ) -> Self {
        Self {
            plane0,
            plane1,
            x,
            y,
            index,
            mirror_h,
            mirror_v,
            hidden,
            palette,
            background,
        }
    }

    /// Colour (0 to 3) of the *source* pixel at (`column`, `row`), flips not applied. 0 is
    /// transparent. `None` when a coordinate is outside 0..=7.
    #[must_use]
    pub const fn source_pixel(&self, column: usize, row: usize) -> Option<u8> {
        if column > 7 || row > 7 {
            return None;
        }

        let bit = (8 * row) + 7 - column;
        #[allow(clippy::cast_possible_truncation)] // Each term is 0 or 1.
        let pixel = ((self.plane0 >> bit) & 1) as u8 + 2 * ((self.plane1 >> bit) & 1) as u8;
        Some(pixel)
    }

    /// Colour (0 to 3) of the pixel *displayed* at tile position (`px`, `py`), flips
    /// applied. `None` when a coordinate is outside 0..=7.
    #[must_use]
    pub const fn displayed_pixel(&self, px: usize, py: usize) -> Option<u8> {
        if px > 7 || py > 7 {
            return None;
        }

        self.source_pixel(
            if self.mirror_h { 7 - px } else { px },
            if self.mirror_v { 7 - py } else { py },
        )
    }

    /// True when both planes are zero, i.e. the tile draws nothing.
    ///
    /// Empty records are still emitted by the core (REQ-CORE-036); filtering is the
    /// consumer's job (SPEC-04).
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        (self.plane0 | self.plane1) == 0
    }

    /// Packs eight CHR bytes of one plane into the 64-bit representation of REQ-CORE-034:
    /// the byte of row `r` goes to bits `8r`..`8r+7`.
    #[must_use]
    pub const fn pack_plane(rows: [u8; 8]) -> u64 {
        u64::from_le_bytes(rows)
    }
}

/// For each band of 8 lines (row 0 = lines 0 to 7), the range of screen x `[low, high)` the
/// tiles of that band could show in: what the game keeps drawn there. A shape that reaches the
/// edge of its band's range may be cut there.
pub type VisibleRows = [(i16, i16); 30];

/// The rows of a standard 256-pixel frame.
pub const STANDARD_ROWS: VisibleRows = [(0, 256); 30];

/// The ordered tile records of one emulated frame, together with the frame palette and the
/// unreliable border widths, handed to the video callback (REQ-CORE-030).
///
/// The records are borrowed: a host that keeps the data must copy it (REQ-CORE-030).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameBatch<'a> {
    /// Tile records in capture order (REQ-CORE-044): chronological by capture line, and for
    /// one line the sprites before the background row.
    pub records: &'a [TileRecord],
    /// The 32 bytes of PPU palette RAM ($3F00-$3F1F) at the end of the frame
    /// (REQ-CORE-045). Each value is a NES colour index, 0 to 63.
    pub palette: [u8; 32],
    /// The margin decoder's own background palette for the frame (16 NES colour indices, as
    /// `$3F00-$3F0F`): background records in sub-palettes 4-7 are in its sub-palettes 0-3.
    /// `None` for a frame without (its records stay in sub-palettes 0-3).
    pub margin_palette: Option<[u8; 16]>,
    /// Width in NES pixels of the unreliable bands on the left *and* right edges
    /// (REQ-CORE-046).
    pub mask_x: i32,
    /// Width in NES pixels of the unreliable bands on the top *and* bottom edges
    /// (REQ-CORE-046).
    pub mask_y: i32,
    /// Widescreen margin of the frame, in NES pixels on each side: 0 for a standard frame.
    /// Background records then reach `-margin - 7` and sprite records `-margin`, and both go
    /// up to `255 + margin`.
    pub margin: i32,
    /// The range of x each band of the frame showed tiles in: [`STANDARD_ROWS`] for a standard
    /// frame, from `-margin` to `256 + margin` at most for a widescreen one.
    pub visible_rows: VisibleRows,
    /// Widescreen margin above and below, in NES pixels: background records then reach
    /// `-margin_y - 8` and `239 + margin_y`. 0 unless a margin decoder fills it.
    pub margin_y: i32,
}

/// The widest widescreen margin, in NES pixels on each side.
pub const MAX_MARGIN: i32 = 512;
/// The tallest widescreen margin above and below, in NES pixels.
pub const MAX_VERTICAL_MARGIN: i32 = 240;
/// Extra tile rows the widest vertical margin adds above and below.
pub const MAX_EXTRA_ROWS: i32 = MAX_VERTICAL_MARGIN / 8;
/// Extra tile columns the widest margin adds on each side.
pub const MAX_EXTRA_COLUMNS: i32 = MAX_MARGIN / 8;
