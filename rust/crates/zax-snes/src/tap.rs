//! The tile tap: what the PPU drew in a frame, tile by tile, and a check that it is complete.
//!
//! When the tap is on, Mesen's PPU records (see `patches/0002-tile-tap.patch`):
//! - for each finished line, the tiles fetched for every background layer and the main screen
//!   as drawn, before colour maths and brightness (the reference);
//! - for each drawn piece of a line, the settings used to draw it;
//! - every 8-pixel sprite slice;
//! - the palette, each time it changed.
//!
//! [`TileCapture::records`] turns that into [`SnesTileRecord`]s, one per tile row. To prove
//! nothing is missing, [`TileCapture::rebuild`] draws the main screen again from the capture
//! alone, with the SNES priority rules, and compares it with the reference pixel by pixel.

// Screen positions, tile columns and colour numbers here are all small (under 512), so the
// casts between integer sizes cannot lose anything.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss
)]

use crate::bgr555_to_rgb;

/// Mesen's `TileData`: one fetched background tile column for a line.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RawTile {
    /// Tile number (bits 0-9), palette (10-12), priority (13), flips (14 across, 15 down).
    pub tilemap: u16,
    pub vscroll: u16,
    /// The row's bit planes, two per word (planes 0-1, 2-3, 4-5, 6-7).
    pub chr: [u16; 4],
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub(crate) struct RawChunk {
    pub scanline: u16,
    pub start_x: u8,
    pub end_x: u8,
    pub bg_mode: u8,
    pub bg3_priority: u8,
    pub main_layers: u8,
    pub direct_color: u8,
    pub forced_blank: u8,
    pub mosaic_size: u8,
    pub mosaic_enabled: u8,
    /// Layers on the sub screen (bits as `main_layers`), blended in by colour maths.
    pub sub_layers: u8,
    pub hscroll: [u16; 4],
    pub cgram_index: u32,
    pub window_masked: [[u8; 32]; 5],
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub(crate) struct RawLine {
    pub scanline: u16,
    pub padding: u16,
    pub tiles: [[RawTile; 33]; 4],
    pub reference: [u16; 256],
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub(crate) struct RawSprite {
    pub fetch_scanline: u16,
    pub x: i16,
    pub palette: u8,
    pub priority: u8,
    pub horizontal_mirror: u8,
    pub vertical_mirror: u8,
    pub chr: [u16; 2],
    pub vram_address: u16,
    pub padding2: u16,
}

// The C++ side checks the same sizes (SnesTileTap.h).
const _: () = assert!(std::mem::size_of::<RawTile>() == 12);
const _: () = assert!(std::mem::size_of::<RawChunk>() == 184);
const _: () = assert!(std::mem::size_of::<RawLine>() == 2100);
const _: () = assert!(std::mem::size_of::<RawSprite>() == 16);

/// The shim's `XsTapView`.
#[repr(C)]
pub(crate) struct RawView {
    pub chunks: *const RawChunk,
    pub chunk_count: u32,
    pub lines: *const RawLine,
    pub line_count: u32,
    pub sprites: *const RawSprite,
    pub sprite_count: u32,
    pub cgram: *const u16,
    pub cgram_snapshots: u32,
}

/// Which layer a tile belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TileLayer {
    /// Background layer 1 to 4, as 0 to 3.
    Background(u8),
    /// A sprite.
    Sprite,
}

/// One row of one tile, as the PPU drew it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnesTileRecord {
    pub layer: TileLayer,
    /// Screen line, 0 at the top of the picture.
    pub y: u16,
    /// Screen x of the tile's left edge; negative or past 255 when partly off screen.
    pub x: i16,
    /// The row's bit planes, two per word, as in [`RawTile::chr`]. Sprites use the first two.
    pub planes: [u16; 4],
    /// 2, 4 or 8 bits per pixel.
    pub bpp: u8,
    /// Palette number (0-7).
    pub palette: u8,
    /// Backgrounds: 0 or 1 (the tile's priority bit). Sprites: 0 to 3.
    pub priority: u8,
    pub flip_x: bool,
    /// Flipped top to bottom; the row is already the one shown on this line.
    pub flip_y: bool,
    /// Backgrounds: the tile number. Sprites: the VRAM word address of the row.
    pub tile: u16,
    /// The background mode the line was drawn in.
    pub bg_mode: u8,
    /// Which palette snapshot applies ([`TileCapture::palette`]).
    pub palette_snapshot: u32,
}

/// How each background layer is drawn in a mode: bits per pixel, the two priorities (tile
/// priority bit clear or set) and where its colours start in the palette.
#[derive(Debug, Clone, Copy)]
struct LayerRule {
    bpp: u8,
    normal: u8,
    high: u8,
    palette_base: u16,
}

const fn rule(bpp: u8, normal: u8, high: u8, palette_base: u16) -> LayerRule {
    LayerRule {
        bpp,
        normal,
        high,
        palette_base,
    }
}

/// The layers and priorities of modes 0 to 4 (Mesen's `RenderMode0` to `RenderMode4`), and the
/// sprite priorities. Larger numbers are in front. Modes 5, 6 and 7 are not rebuilt yet.
fn mode_rules(mode: u8, bg3_priority: bool) -> Option<([Option<LayerRule>; 4], [u8; 4])> {
    Some(match mode {
        0 => (
            [
                Some(rule(2, 8, 11, 0)),
                Some(rule(2, 7, 10, 32)),
                Some(rule(2, 2, 5, 64)),
                Some(rule(2, 1, 4, 96)),
            ],
            [3, 6, 9, 12],
        ),
        1 => (
            [
                Some(rule(4, 6, 9, 0)),
                Some(rule(4, 5, 8, 0)),
                Some(rule(2, 1, if bg3_priority { 11 } else { 3 }, 0)),
                None,
            ],
            [2, 4, 7, 10],
        ),
        2 => (
            [Some(rule(4, 3, 7, 0)), Some(rule(4, 1, 5, 0)), None, None],
            [2, 4, 6, 8],
        ),
        3 => (
            [Some(rule(8, 3, 7, 0)), Some(rule(4, 1, 5, 0)), None, None],
            [2, 4, 6, 8],
        ),
        4 => (
            [Some(rule(8, 3, 7, 0)), Some(rule(2, 1, 5, 0)), None, None],
            [2, 4, 6, 8],
        ),
        _ => return None,
    })
}

/// The colour number of pixel `shift` (7 = leftmost) in a row, as Mesen's `GetTilePixelColor`.
fn pixel_color(chr: [u16; 4], bpp: u8, shift: u8) -> u8 {
    let pair = |word: u16| -> u8 {
        let low = (word >> shift) & 0x01;
        let high = (word >> (7 + shift)) & 0x02;
        (low | high) as u8
    };
    let mut color = pair(chr[0]);
    if bpp >= 4 {
        color |= pair(chr[1]) << 2;
    }
    if bpp == 8 {
        color |= pair(chr[2]) << 4;
        color |= pair(chr[3]) << 6;
    }
    color
}

/// The result of [`TileCapture::rebuild`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rebuild {
    pub width: u32,
    pub height: u32,
    /// The main screen as the PPU drew it (before colour maths), red, green, blue.
    pub reference: Vec<u8>,
    /// The same, drawn again from the capture alone.
    pub rebuilt: Vec<u8>,
    /// Pixels compared and pixels that differ.
    pub checked: u32,
    pub mismatched: u32,
    /// Pixels not compared: lines in modes 5, 6 and 7, which are not rebuilt yet.
    pub skipped: u32,
}

/// Everything the tap recorded during one frame.
#[derive(Debug, Clone, Default)]
pub struct TileCapture {
    pub(crate) chunks: Vec<RawChunk>,
    pub(crate) lines: Vec<RawLine>,
    pub(crate) sprites: Vec<RawSprite>,
    cgram: Vec<u16>,
}

impl TileCapture {
    /// Copies what the shim holds.
    ///
    /// # Safety
    ///
    /// The view's pointers must be valid for their counts (the shim keeps them valid until
    /// the next frame).
    pub(crate) unsafe fn from_view(view: &RawView) -> Self {
        // SAFETY: the caller guarantees each pointer is valid for its count; a count of 0
        // never reads its pointer.
        unsafe {
            Self {
                chunks: copy(view.chunks, view.chunk_count as usize),
                lines: copy(view.lines, view.line_count as usize),
                sprites: copy(view.sprites, view.sprite_count as usize),
                cgram: copy(view.cgram, view.cgram_snapshots as usize * 256),
            }
        }
    }

    /// Lines recorded.
    #[must_use]
    pub fn line_count(&self) -> usize {
        self.lines.len()
    }

    /// Sprite slices recorded.
    #[must_use]
    pub fn sprite_count(&self) -> usize {
        self.sprites.len()
    }

    /// The 256 colours (15-bit BGR) of a palette snapshot.
    #[must_use]
    pub fn palette(&self, snapshot: u32) -> Option<&[u16]> {
        let start = snapshot as usize * 256;
        self.cgram.get(start..start + 256)
    }

    /// The colour the sky shows in the finished picture `frame`: the most common colour of the
    /// pixels where the main screen shows only the backdrop (palette entry 0). A game that
    /// paints its sky with colour maths (Super Mario World, Donkey Kong Country) keeps entry 0
    /// black and blends the colour in, so the finished picture is where its sky is. `None` when
    /// no pixel shows the backdrop.
    #[must_use]
    pub fn sky_color(&self, frame: &crate::Frame) -> Option<[u8; 3]> {
        let first_row: u16 = if frame.height >= 239 { 0 } else { 1 };
        let mut counts: std::collections::HashMap<[u8; 3], u32> = std::collections::HashMap::new();
        for line in &self.lines {
            let Some(chunk) = self.last_chunk(line.scanline) else {
                continue;
            };
            let Some(palette) = self.palette(chunk.cgram_index) else {
                continue;
            };
            let Some(y) = line.scanline.checked_sub(first_row) else {
                continue;
            };
            if u32::from(y) >= frame.height {
                continue;
            }
            for (x, &pixel) in line.reference.iter().enumerate().take(frame.width as usize) {
                if pixel == palette[0] {
                    if let Some(color) = frame.pixel(x as u32, u32::from(y)) {
                        *counts.entry(color).or_default() += 1;
                    }
                }
            }
        }
        // Ties go to the smaller colour, so the result does not depend on the map's order.
        counts
            .into_iter()
            .max_by_key(|(color, count)| (*count, std::cmp::Reverse(*color)))
            .map(|(color, _)| color)
    }

    /// The last chunk drawn on `scanline`: the settings a line's tiles are filed under.
    fn last_chunk(&self, scanline: u16) -> Option<&RawChunk> {
        self.chunks.iter().rev().find(|c| c.scanline == scanline)
    }

    /// Every tile row the PPU drew: background tiles of the layers on the main screen, then
    /// sprite slices.
    #[must_use]
    pub fn records(&self) -> Vec<SnesTileRecord> {
        let mut records = Vec::new();
        for line in &self.lines {
            let Some(chunk) = self.last_chunk(line.scanline) else {
                continue;
            };
            if chunk.forced_blank != 0 {
                continue;
            }
            let y = line.scanline.saturating_sub(1);
            let Some((layers, _)) = mode_rules(chunk.bg_mode, chunk.bg3_priority != 0) else {
                continue;
            };
            for (index, rule) in layers.iter().enumerate() {
                let Some(rule) = rule else { continue };
                if chunk.main_layers & (1 << index) == 0 {
                    continue;
                }
                let fine = (chunk.hscroll[index] & 0x07) as i16;
                for (column, tile) in line.tiles[index].iter().enumerate() {
                    let x = column as i16 * 8 - fine;
                    if x >= 256 {
                        continue;
                    }
                    records.push(SnesTileRecord {
                        layer: TileLayer::Background(index as u8),
                        y,
                        x,
                        planes: tile.chr,
                        bpp: rule.bpp,
                        palette: ((tile.tilemap >> 10) & 0x07) as u8,
                        priority: u8::from(tile.tilemap & 0x2000 != 0),
                        flip_x: tile.tilemap & 0x4000 != 0,
                        flip_y: tile.tilemap & 0x8000 != 0,
                        tile: tile.tilemap & 0x03ff,
                        bg_mode: chunk.bg_mode,
                        palette_snapshot: chunk.cgram_index,
                    });
                }
            }
        }
        for sprite in &self.sprites {
            let draw_line = sprite.fetch_scanline + 1;
            let Some(chunk) = self.last_chunk(draw_line) else {
                continue;
            };
            records.push(SnesTileRecord {
                layer: TileLayer::Sprite,
                y: draw_line - 1,
                x: sprite.x,
                planes: [sprite.chr[0], sprite.chr[1], 0, 0],
                bpp: 4,
                palette: sprite.palette,
                priority: sprite.priority,
                flip_x: sprite.horizontal_mirror != 0,
                flip_y: sprite.vertical_mirror != 0,
                tile: sprite.vram_address,
                bg_mode: chunk.bg_mode,
                palette_snapshot: chunk.cgram_index,
            });
        }
        records
    }

    /// Draws the main screen again from the capture and compares it with the reference.
    #[must_use]
    pub fn rebuild(&self) -> Rebuild {
        let height: u32 = if self.lines.iter().any(|l| l.scanline > 224) {
            239
        } else {
            224
        };
        let width = 256;
        let size = (width * height * 3) as usize;
        let mut result = Rebuild {
            width,
            height,
            reference: vec![0; size],
            rebuilt: vec![0; size],
            checked: 0,
            mismatched: 0,
            skipped: 0,
        };

        for line in &self.lines {
            let y = u32::from(line.scanline) - 1;
            if y >= height {
                continue;
            }
            let sprites = self.sprite_line(line.scanline);
            let mut mosaic = [MosaicLatch::default(); 4];
            for chunk in self.chunks.iter().filter(|c| c.scanline == line.scanline) {
                for x in chunk.start_x..=chunk.end_x {
                    let reference = line.reference[usize::from(x)];
                    let rebuilt = self.pixel(chunk, line, &sprites, &mut mosaic, x);
                    let at = ((y * width + u32::from(x)) * 3) as usize;
                    result.reference[at..at + 3].copy_from_slice(&bgr555_to_rgb(reference));
                    if let Some(colour) = rebuilt {
                        result.rebuilt[at..at + 3].copy_from_slice(&bgr555_to_rgb(colour));
                        result.checked += 1;
                        if colour != reference {
                            result.mismatched += 1;
                        }
                    } else {
                        result.rebuilt[at..at + 3].copy_from_slice(&bgr555_to_rgb(reference));
                        result.skipped += 1;
                    }
                }
            }
        }
        result
    }

    /// The sprite pixels of a drawn line, built from the slices fetched on the line before.
    /// Later slices cover earlier ones, as in Mesen's `FetchSpriteTile`.
    fn sprite_line(&self, scanline: u16) -> Vec<Option<(u8, u8, u8)>> {
        let mut pixels: Vec<Option<(u8, u8, u8)>> = vec![None; 256];
        for sprite in self
            .sprites
            .iter()
            .filter(|s| s.fetch_scanline + 1 == scanline)
        {
            for offset in 0..8i16 {
                let x = sprite.x + offset;
                if !(0..=255).contains(&x) {
                    continue;
                }
                let column = if sprite.horizontal_mirror != 0 {
                    7 - offset
                } else {
                    offset
                };
                let chr = [sprite.chr[0], sprite.chr[1], 0, 0];
                let color = pixel_color(chr, 4, (7 - column) as u8);
                if color != 0 {
                    pixels[x as usize] = Some((color, sprite.palette, sprite.priority));
                }
            }
        }
        pixels
    }

    /// One main screen pixel, or `None` for a mode that is not rebuilt.
    fn pixel(
        &self,
        chunk: &RawChunk,
        line: &RawLine,
        sprites: &[Option<(u8, u8, u8)>],
        mosaic: &mut [MosaicLatch; 4],
        x: u8,
    ) -> Option<u16> {
        if chunk.forced_blank != 0 {
            return Some(0);
        }
        let (layers, sprite_priorities) = mode_rules(chunk.bg_mode, chunk.bg3_priority != 0)?;
        let palette = self.palette(chunk.cgram_index)?;
        let masked =
            |layer: usize| chunk.window_masked[layer][usize::from(x >> 3)] & (1 << (x & 7)) != 0;

        let mut best: Option<(u8, u16)> = None;
        let mut offer = |priority: u8, colour: u16| {
            if best.is_none_or(|(current, _)| current < priority) {
                best = Some((priority, colour));
            }
        };

        if chunk.main_layers & 0x10 != 0 && !masked(4) {
            if let Some((color, sprite_palette, priority)) = sprites[usize::from(x)] {
                let index = 128 + usize::from(sprite_palette) * 16 + usize::from(color);
                offer(sprite_priorities[usize::from(priority)], palette[index]);
            }
        }

        for (index, rule) in layers.iter().enumerate() {
            let Some(rule) = rule else { continue };
            let hscroll = chunk.hscroll[index];
            let column = ((u16::from(x) + (hscroll & 0x07)) >> 3) as usize;
            let tile = line.tiles[index][column];
            let flip = tile.tilemap & 0x4000 != 0;
            let offset = ((u16::from(x) + hscroll) & 0x07) as u8;
            let shift = if flip { offset } else { 7 - offset };
            let mut color = pixel_color(tile.chr, rule.bpp, shift);
            let mut tile_palette = ((tile.tilemap >> 10) & 0x07) as u8;
            let mut priority = if tile.tilemap & 0x2000 != 0 {
                rule.high
            } else {
                rule.normal
            };

            // Mosaic repeats the first pixel of each block across it.
            if chunk.mosaic_enabled & (1 << index) != 0 && chunk.mosaic_size > 1 {
                let latch = &mut mosaic[index];
                if x == chunk.start_x {
                    latch.counter = x % chunk.mosaic_size;
                }
                if latch.counter == 0 {
                    *latch = MosaicLatch {
                        counter: 0,
                        color,
                        palette: tile_palette,
                        priority,
                    };
                } else {
                    color = latch.color;
                    tile_palette = latch.palette;
                    priority = latch.priority;
                }
                latch.counter += 1;
                if latch.counter == chunk.mosaic_size {
                    latch.counter = 0;
                }
            }

            // Mesen checks the main screen and window per pixel, after mosaic.
            if color == 0 || chunk.main_layers & (1 << index) == 0 || masked(index) {
                continue;
            }
            let layer_colour = if rule.bpp == 8 && chunk.direct_color != 0 {
                direct_color(tile_palette, color)
            } else if rule.bpp == 8 {
                palette[usize::from(rule.palette_base) + usize::from(color)]
            } else {
                palette[usize::from(rule.palette_base)
                    + usize::from(tile_palette) * (1 << rule.bpp)
                    + usize::from(color)]
            };
            offer(priority, layer_colour);
        }

        Some(best.map_or(palette[0], |(_, colour)| colour))
    }
}

/// Mosaic state of one layer across a line, as Mesen keeps it.
#[derive(Debug, Clone, Copy, Default)]
struct MosaicLatch {
    counter: u8,
    color: u8,
    palette: u8,
    priority: u8,
}

/// 256-colour layers in direct colour mode take the colour from the pixel itself.
fn direct_color(palette: u8, color: u8) -> u16 {
    let (palette, color) = (u16::from(palette), u16::from(color));
    ((((color & 0x07) << 1) | (palette & 0x01)) << 1)
        | (((color & 0x38) | ((palette & 0x02) << 1)) << 4)
        | (((color & 0xC0) | ((palette & 0x04) << 3)) << 7)
}

unsafe fn copy<T: Copy>(pointer: *const T, count: usize) -> Vec<T> {
    if count == 0 || pointer.is_null() {
        return Vec::new();
    }
    // SAFETY: the caller guarantees `pointer` is valid for `count` items.
    unsafe { std::slice::from_raw_parts(pointer, count) }.to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pixel_colors_follow_the_bit_planes() {
        // Leftmost pixel (shift 7): plane 0 set, plane 1 clear, plane 2 set.
        let chr = [0x0080, 0x0080, 0, 0];
        assert_eq!(pixel_color(chr, 2, 7), 1);
        assert_eq!(pixel_color(chr, 4, 7), 5);
        assert_eq!(pixel_color(chr, 4, 6), 0);
    }

    #[test]
    fn only_modes_0_to_4_are_rebuilt() {
        assert!(mode_rules(1, false).is_some());
        assert!(mode_rules(7, false).is_none());
        let (layers, _) = mode_rules(1, true).unwrap();
        assert_eq!(layers[2].unwrap().high, 11);
    }
}
