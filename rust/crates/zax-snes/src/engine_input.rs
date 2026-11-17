//! Turns a frame's tile capture into what the 3D engine reads: whole 8x8 tile records, one
//! list per background layer, and the frame's 256 colours.
//!
//! The PPU records one row of a tile per line ([`crate::tap`]). Here the rows of each tile are
//! put back together: a background row sits at row `(line + vertical scroll) mod 8` of its tile
//! (as in Mesen's `GetChrData`), a sprite row at the row its VRAM address points to. Tiles cut
//! by the top or bottom of the picture keep only the rows that were shown.
//!
//! Each background layer goes to its own engine (the engine knows one background grid, as the
//! NES has one layer). Sprites go with layer 1, the play layer of most games.

// Screen positions, rows and palette numbers here are all small, so the casts between
// integer sizes cannot lose anything.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss
)]

use std::collections::BTreeMap;

use zax_engine3d::input::{FrameBatch, TileRecord, STANDARD_ROWS};
use zax_engine3d::palette::Rgb24;

use crate::bgr555_to_rgb;
use crate::tap::TileCapture;

/// One background layer's tiles for its engine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LayerInput {
    /// Background layer 1 to 4, as 0 to 3.
    pub layer: u8,
    /// The layer's tiles, sprites first for layer 0, then the background tiles top to bottom
    /// and left to right.
    pub records: Vec<TileRecord>,
}

/// A frame ready for the engines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineInput {
    /// The layers drawn on the main screen, in layer order. Empty for a frame the tap cannot
    /// read as tiles (Mode 7, the hi-res modes, forced blank all frame).
    pub layers: Vec<LayerInput>,
    /// The frame's 256 colours, from the palette in force at the end of the frame.
    pub colors: Box<[Rgb24; 256]>,
    /// The background mode at the end of the frame.
    pub bg_mode: u8,
}

impl EngineInput {
    /// The engine batch of one layer.
    #[must_use]
    pub fn batch<'a>(&'a self, layer: &'a LayerInput) -> FrameBatch<'a> {
        FrameBatch {
            records: &layer.records,
            palette: [0; 32],
            margin_palette: None,
            colors: Some(&self.colors),
            mask_x: 0,
            mask_y: 0,
            margin: 0,
            visible_rows: STANDARD_ROWS,
            margin_y: 0,
        }
    }
}

/// A background tile's key: its top, its x and its tile map entry.
type BackgroundKey = (i16, i16, u16);

/// A sprite tile's key: its top, its x, its VRAM address and its palette, priority and flips.
type SpriteKey = (i16, i16, u16, u8, u8, bool, bool);

/// Rows gathered for one tile.
#[derive(Debug, Clone, Copy, Default)]
struct Builder {
    planes: [u64; 8],
    flip_x: bool,
    flip_y: bool,
    palette: u8,
    order: usize,
}

impl Builder {
    /// Puts the planes of one row (two per word, low byte the even plane) at source row `row`.
    fn add_row(&mut self, chr: &[u16], planes: usize, row: u32) {
        for plane in 0..planes {
            let word = chr[plane / 2];
            let byte = if plane % 2 == 0 {
                word & 0xFF
            } else {
                word >> 8
            };
            self.planes[plane] |= u64::from(byte) << (8 * row);
        }
    }

    fn record(&self, x: i16, y: i16, index: i32, background: bool) -> TileRecord {
        let p = &self.planes;
        TileRecord::new(
            p[0],
            p[1],
            x,
            y,
            index,
            self.flip_x,
            self.flip_y,
            false,
            self.palette,
            background,
        )
        .with_upper([p[2], p[3], p[4], p[5], p[6], p[7]])
    }
}

/// Bits per pixel, priorities and palette start of each background layer per mode; `None` for
/// a layer the mode does not have. The same table as the rebuild check's.
fn layer_bpp(mode: u8) -> Option<[Option<(u8, u16)>; 4]> {
    Some(match mode {
        0 => [Some((2, 0)), Some((2, 32)), Some((2, 64)), Some((2, 96))],
        1 => [Some((4, 0)), Some((4, 0)), Some((2, 0)), None],
        2 => [Some((4, 0)), Some((4, 0)), None, None],
        3 => [Some((8, 0)), Some((4, 0)), None, None],
        4 => [Some((8, 0)), Some((2, 0)), None, None],
        _ => return None,
    })
}

impl TileCapture {
    /// The frame as the 3D engines read it (see the module notes).
    #[must_use]
    pub fn engine_input(&self) -> EngineInput {
        let last = self.chunks.iter().max_by_key(|c| (c.scanline, c.end_x));
        let mut colors = Box::new([Rgb24::default(); 256]);
        if let Some(palette) = last.and_then(|c| self.palette(c.cgram_index)) {
            for (color, &bgr) in colors.iter_mut().zip(palette) {
                let [r, g, b] = bgr555_to_rgb(bgr);
                *color = Rgb24::new(r, g, b);
            }
        }
        let bg_mode = last.map_or(0, |c| c.bg_mode);

        let (layers, used, sprites_on) = self.background_tiles();
        let sprites = if sprites_on {
            self.sprite_tiles()
        } else {
            BTreeMap::new()
        };

        let mut out = Vec::new();
        for (layer, tiles) in layers.iter().enumerate() {
            if !used[layer] {
                continue;
            }
            let mut records = Vec::new();
            if layer == 0 {
                // Sprites first, in the order they were met; the index is that order.
                let mut in_order: Vec<_> = sprites.iter().collect();
                in_order.sort_by_key(|(_, builder)| builder.order);
                for (index, ((top, x, ..), builder)) in in_order.into_iter().enumerate() {
                    records.push(builder.record(*x, *top, index as i32, false));
                }
            }
            for (index, ((top, x, _), builder)) in tiles.iter().enumerate() {
                records.push(builder.record(*x, *top, index as i32, true));
            }
            out.push(LayerInput {
                layer: layer as u8,
                records,
            });
        }

        EngineInput {
            layers: out,
            colors,
            bg_mode,
        }
    }
    /// The background rows put together per layer, which layers were drawn, and whether
    /// sprites were on the main screen.
    #[allow(clippy::type_complexity)] // Three plain results.
    fn background_tiles(&self) -> ([BTreeMap<BackgroundKey, Builder>; 4], [bool; 4], bool) {
        let mut layers: [BTreeMap<BackgroundKey, Builder>; 4] = Default::default();
        let mut used = [false; 4];
        let mut sprites_on = false;
        for line in &self.lines {
            let Some(chunk) = self
                .chunks
                .iter()
                .rev()
                .find(|c| c.scanline == line.scanline)
            else {
                continue;
            };
            if chunk.forced_blank != 0 {
                continue;
            }
            sprites_on |= chunk.main_layers & 0x10 != 0;
            let Some(rules) = layer_bpp(chunk.bg_mode) else {
                continue;
            };
            let y = line.scanline as i16 - 1;
            for (layer, rule) in rules.iter().enumerate() {
                let Some((bpp, base)) = *rule else { continue };
                if chunk.main_layers & (1 << layer) == 0 {
                    continue;
                }
                used[layer] = true;
                let fine = (chunk.hscroll[layer] & 0x07) as i16;
                for (column, tile) in line.tiles[layer].iter().enumerate() {
                    let x = column as i16 * 8 - fine;
                    if x >= 256 {
                        continue;
                    }
                    let shown_row = (line.scanline.wrapping_add(tile.vscroll) & 0x07) as i16;
                    let flip_y = tile.tilemap & 0x8000 != 0;
                    let source_row = if flip_y { 7 - shown_row } else { shown_row };
                    let palette_number = (tile.tilemap >> 10) & 0x07;
                    let palette = if bpp == 8 {
                        0
                    } else {
                        base + palette_number * (1 << bpp)
                    };
                    let key = (y - shown_row, x, tile.tilemap);
                    let order = layers[layer].len();
                    let builder = layers[layer].entry(key).or_insert_with(|| Builder {
                        flip_x: tile.tilemap & 0x4000 != 0,
                        flip_y,
                        palette: palette.min(255) as u8,
                        order,
                        ..Builder::default()
                    });
                    builder.add_row(&tile.chr, usize::from(bpp), source_row as u32);
                }
            }
        }

        (layers, used, sprites_on)
    }

    /// The sprite rows put together per tile.
    fn sprite_tiles(&self) -> BTreeMap<SpriteKey, Builder> {
        let mut sprites: BTreeMap<SpriteKey, Builder> = BTreeMap::new();
        {
            for sprite in &self.sprites {
                let y = sprite.fetch_scanline as i16; // drawn on the next line: screen y
                let source_row = sprite.vram_address & 0x07;
                let flip_y = sprite.vertical_mirror != 0;
                let shown_row = if flip_y { 7 - source_row } else { source_row } as i16;
                let key = (
                    y - shown_row,
                    sprite.x,
                    sprite.vram_address & !0x07,
                    sprite.palette,
                    sprite.priority,
                    sprite.horizontal_mirror != 0,
                    flip_y,
                );
                let order = sprites.len();
                let builder = sprites.entry(key).or_insert_with(|| Builder {
                    flip_x: sprite.horizontal_mirror != 0,
                    flip_y,
                    palette: 128 + sprite.palette * 16,
                    order,
                    ..Builder::default()
                });
                builder.add_row(&sprite.chr, 4, u32::from(source_row));
            }
        }

        sprites
    }
}
