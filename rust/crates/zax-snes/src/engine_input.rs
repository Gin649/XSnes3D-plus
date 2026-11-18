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
use crate::tap::{RawChunk, TileCapture};

/// One background layer's tiles for its engine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LayerInput {
    /// Background layer 1 to 4, as 0 to 3, or [`BG3_FRONT`].
    pub layer: u8,
    /// The layer's tiles, sprites first for layer 0, then the background tiles top to bottom
    /// and left to right.
    pub records: Vec<TileRecord>,
}

/// `LayerInput::layer` of mode 1's background 3 tiles that its priority bit puts in front of
/// everything (the text or status bar of most games). The layer's other tiles stay layer 2,
/// at the back, as on the console.
pub const BG3_FRONT: u8 = 4;

/// `LayerInput::layer` of the sky: the colour each line of the finished picture shows where
/// no layer covers it. Games paint their sky gradients by changing that colour line by line
/// (or by blending a colour in), which one background colour cannot show.
pub const BACKDROP: u8 = 5;

/// A frame ready for the engines.
#[derive(Debug, Clone, PartialEq)]
pub struct EngineInput {
    /// The layers drawn on the main or the sub screen, in layer order. Empty for a frame the tap cannot
    /// read as tiles (the hi-res modes, forced blank all frame).
    pub layers: Vec<LayerInput>,
    /// The frame's 256 colours, from the palette in force at the end of the frame.
    pub colors: Box<[Rgb24; 256]>,
    /// The background mode at the end of the frame.
    pub bg_mode: u8,
    /// Mode 1's background 3 priority bit, set on any line that drew background 3: its high
    /// priority tiles then go in front of everything (the status bar or text of most games).
    pub bg3_priority: bool,
    /// For each screen line (0 up), how much further away it is than the nearest line of its
    /// Mode 7 floor (1 for the nearest, 2 for twice as far, and so on), from how much of the
    /// floor each pixel covers; `None` on lines that are not a Mode 7 floor. A Mode 7 picture
    /// seen from above (a map) has every line at 1.
    pub floor: Vec<Option<f32>>,
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

/// A Mode 7 picture counts as a floor going into the distance when its furthest line covers
/// this much more than its nearest.
const FLOOR_MIN_SPREAD: f32 = 1.1;

/// The furthest a floor line is put, in times the distance of its nearest line (the lines by
/// the horizon would otherwise go almost endlessly far).
const FLOOR_MAX_DISTANCE: f32 = 3.0;

/// The tile map entry in the key of a Mode 7 screen tile, which has none of its own.
const MODE7_TILE: u16 = 0xFFFF;

/// A sprite tile's key: its top, its x, its VRAM address and its palette, priority and flips.
type SpriteKey = (i16, i16, u16, u8, u8, bool, bool);

/// Rows gathered for one tile.
#[derive(Debug, Clone, Copy, Default)]
struct Builder {
    planes: [u64; 8],
    flip_x: bool,
    flip_y: bool,
    /// A sprite drawn behind background layer 1 (the NES "behind background" flag).
    hidden: bool,
    palette: u8,
    order: usize,
    /// The palette snapshot in force when the tile was drawn; `None` for a tile whose colours
    /// are the whole palette (8 bits per pixel, Mode 7).
    snapshot: Option<u32>,
}

impl Builder {
    /// Puts the planes of one row (two per word, low byte the even plane) at source row `row`.
    /// Only the pixels whose bit is set in `keep` are kept (bit 7 is the left pixel of the
    /// source row, as in the planes).
    fn add_row(&mut self, chr: &[u16], planes: usize, row: u32, keep: u8) {
        for plane in 0..planes {
            let word = chr[plane / 2];
            let byte = if plane % 2 == 0 {
                word & 0xFF
            } else {
                word >> 8
            };
            self.planes[plane] |= u64::from(byte & u16::from(keep)) << (8 * row);
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
            self.hidden,
            self.palette,
            background,
        )
        .with_upper([p[2], p[3], p[4], p[5], p[6], p[7]])
    }
}

/// How far apart two colours are (the sum of the squared differences).
fn distance(a: [u8; 3], b: [u8; 3]) -> u32 {
    a.iter()
        .zip(b)
        .map(|(&x, y)| (i32::from(x) - i32::from(y)).pow(2) as u32)
        .sum()
}

/// The pixels of an 8 pixel row drawn from screen x `x` that are on screen and not hidden
/// (`hidden` gets the screen x), as a bit mask in the planes' order: bit 7 is the source row's
/// left pixel, which a mirrored tile draws on the right.
fn shown_pixels(x: i16, flip_x: bool, hidden: impl Fn(u16) -> bool) -> u8 {
    let mut keep = 0u8;
    for i in 0..8 {
        let screen_x = x + i;
        if !(0..256).contains(&screen_x) || hidden(screen_x as u16) {
            continue;
        }
        let bit = if flip_x { i } else { 7 - i };
        keep |= 1 << bit;
    }
    keep
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

/// True when a sprite of priority `priority` (0 to 3) is drawn behind background layer 1's
/// ordinary tiles in background mode `mode` (the sprite priorities against layer 1's lower
/// priority, as Mesen's `RenderMode0` to `RenderMode4` set them).
fn behind_layer_1(mode: u8, priority: u8) -> bool {
    let (sprites, layer_1): ([u8; 4], u8) = match mode {
        0 => ([3, 6, 9, 12], 8),
        1 => ([2, 4, 7, 10], 6),
        _ => ([2, 4, 6, 8], 3),
    };
    sprites[usize::from(priority & 3)] < layer_1
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
        // Games often set the bit only for their status bar's lines (Super Mario World), so a
        // frame counts when any mode 1 line drew background 3 with it.
        let bg3_priority = self
            .chunks
            .iter()
            .any(|c| c.bg_mode == 1 && c.bg3_priority != 0 && c.main_layers & 0x04 != 0);

        let (layers, used, sprites_on) = self.background_tiles();
        let sprites = if sprites_on {
            self.sprite_tiles()
        } else {
            BTreeMap::new()
        };

        let mut out = Vec::new();
        let mut snapshots: Vec<Vec<Option<u32>>> = Vec::new();
        for (layer, tiles) in layers.iter().enumerate() {
            // Sprites ride with layer 1; on a frame without it they still get layer 1's
            // engine, alone.
            let sprites_alone = layer == 0 && !used[0] && !sprites.is_empty();
            if !used[layer] && !sprites_alone {
                continue;
            }
            let mut records = Vec::new();
            let mut layer_snapshots = Vec::new();
            if layer == 0 {
                // Sprites first, in the order they were met; the index is that order.
                let mut in_order: Vec<_> = sprites.iter().collect();
                in_order.sort_by_key(|(_, builder)| builder.order);
                for (index, ((top, x, ..), builder)) in in_order.into_iter().enumerate() {
                    records.push(builder.record(*x, *top, index as i32, false));
                    layer_snapshots.push(builder.snapshot);
                }
            }
            for (index, ((top, x, _), builder)) in tiles.iter().enumerate() {
                records.push(builder.record(*x, *top, index as i32, true));
                layer_snapshots.push(builder.snapshot);
            }
            out.push(LayerInput {
                layer: layer as u8,
                records,
            });
            snapshots.push(layer_snapshots);
        }
        if let Some(last) = last {
            self.match_palettes(&mut out, &snapshots, &mut colors, last.cgram_index);
        }

        EngineInput {
            layers: out,
            colors,
            bg_mode,
            bg3_priority,
            floor: self.floor_distances(),
        }
    }
    /// [`EngineInput::floor`]: the Mode 7 lines of background 1, cut into floors (runs of
    /// lines next to each other), each line's distance as its scale over the floor's smallest.
    fn floor_distances(&self) -> Vec<Option<f32>> {
        let mut scale = vec![None; 240];
        for line in self.mode7.iter().filter(|l| l.layer == 0) {
            let [a, _, c, _] = line.matrix;
            let step = f32::hypot(f32::from(a), f32::from(c)) / 256.0;
            let y = usize::from(line.scanline).wrapping_sub(1);
            if step > 0.0 && y < scale.len() {
                scale[y] = Some(step);
            }
        }
        let mut floor = vec![None; scale.len()];
        let mut start = 0;
        while start < scale.len() {
            if scale[start].is_none() {
                start += 1;
                continue;
            }
            let end = (start..scale.len())
                .find(|&y| scale[y].is_none())
                .unwrap_or(scale.len());
            let steps: Vec<f32> = scale[start..end].iter().flatten().copied().collect();
            let nearest = steps.iter().copied().fold(f32::MAX, f32::min);
            let furthest = steps.iter().copied().fold(0.0, f32::max);
            // A floor seen from straight above has the same scale on every line.
            let tilted = furthest > nearest * FLOOR_MIN_SPREAD;
            for y in start..end {
                floor[y] = Some(if tilted {
                    scale[y].map_or(1.0, |s| (s / nearest).clamp(1.0, FLOOR_MAX_DISTANCE))
                } else {
                    1.0
                });
            }
            start = end;
        }
        floor
    }

    /// Games change colours partway down the picture (Mario Kart's top strip, many status
    /// bars), but the engines have one palette per frame: the one in force at the end. A tile
    /// drawn with other colours is pointed at entries that hold its colours: the same colours
    /// somewhere else in the palette, or entries no tile of the frame uses, filled in.
    fn match_palettes(
        &self,
        layers: &mut [LayerInput],
        snapshots: &[Vec<Option<u32>>],
        colors: &mut [Rgb24; 256],
        last_snapshot: u32,
    ) {
        let rgb = |bgr: u16| {
            let [r, g, b] = bgr555_to_rgb(bgr);
            Rgb24::new(r, g, b)
        };
        // The colour values (1 up) a record shows.
        let values_of = |record: &TileRecord| {
            let mut values = [false; 256];
            for row in 0..8 {
                for column in 0..8 {
                    if let Some(value) = record.source_pixel(column, row) {
                        values[usize::from(value)] = value != 0;
                    }
                }
            }
            values
        };
        let shows = |record: &TileRecord, snapshot: u32, colors: &[Rgb24; 256], start: usize| {
            let Some(palette) = self.palette(snapshot) else {
                return true;
            };
            values_of(record).iter().enumerate().all(|(value, &used)| {
                let from = usize::from(record.palette) + value;
                let to = start + value;
                !used || (from < 256 && to < 256 && colors[to] == rgb(palette[from]))
            })
        };

        // Entries the tiles already right show stay as they are.
        let mut taken = [false; 256];
        taken[0] = true;
        let mut wrong = Vec::new();
        for (layer_index, layer) in layers.iter().enumerate() {
            for (record_index, record) in layer.records.iter().enumerate() {
                let snapshot = snapshots[layer_index][record_index];
                match snapshot {
                    Some(snapshot)
                        if snapshot != last_snapshot
                            && !shows(record, snapshot, colors, usize::from(record.palette)) =>
                    {
                        wrong.push((layer_index, record_index, snapshot));
                    }
                    _ => {
                        for (value, &used) in values_of(record).iter().enumerate() {
                            if used {
                                taken[(usize::from(record.palette) + value).min(255)] = true;
                            }
                        }
                    }
                }
            }
        }

        // One new place per palette block of a snapshot.
        let mut moved: Vec<((u32, u8), u8)> = Vec::new();
        for (layer_index, record_index, snapshot) in wrong {
            let record = &layers[layer_index].records[record_index];
            let key = (snapshot, record.palette);
            if let Some(&(_, start)) = moved.iter().find(|(k, _)| *k == key) {
                if shows(record, snapshot, colors, usize::from(start)) {
                    layers[layer_index].records[record_index].palette = start;
                    continue;
                }
            }
            let Some(palette) = self.palette(snapshot) else {
                continue;
            };
            let values = values_of(record);
            let size = values.iter().rposition(|&used| used).unwrap_or(0) + 1;
            // The same colours elsewhere, or free entries to hold them.
            let start = (0..=256 - size)
                .find(|&start| shows(record, snapshot, colors, start))
                .or_else(|| {
                    (0..=256 - size).find(|&start| {
                        values
                            .iter()
                            .enumerate()
                            .all(|(value, &used)| !used || !taken[start + value])
                    })
                });
            let Some(start) = start else { continue };
            // A block of 16 colours or fewer: all of it moves, so later tiles of the same
            // block find their colours too.
            let block = if size <= 16 {
                usize::from(record.palette)..usize::from(record.palette) + size
            } else {
                0..0
            };
            for (value, &used) in values.iter().enumerate() {
                let from = usize::from(record.palette) + value;
                let in_block = block.contains(&from) && value != 0;
                if (used || in_block) && from < 256 && !taken[start + value] {
                    colors[start + value] = rgb(palette[from]);
                    taken[start + value] = true;
                }
            }
            layers[layer_index].records[record_index].palette = start as u8;
            moved.push((key, start as u8));
        }
    }

    /// Adds the sky to `input` as a layer of its own ([`BACKDROP`]), from the finished
    /// picture `frame`: whole tiles whose rows are each line's sky colour. Its colours go in
    /// palette entries no tile of the frame uses (all layers share one palette). Nothing is
    /// added when the sky is one colour all the way down (the scene's background colour shows
    /// it then) or cannot be found.
    pub fn add_backdrop(&self, input: &mut EngineInput, frame: &crate::Frame) {
        let sky = self.sky_by_line(frame);
        let Some(&(_, first)) = sky.first() else {
            return;
        };
        if sky.iter().all(|(_, color)| *color == first) {
            return;
        }

        // The palette entries the frame's tiles show; entry 0 is the backdrop's own.
        let mut taken = [false; 256];
        taken[0] = true;
        for record in input.layers.iter().flat_map(|layer| &layer.records) {
            for row in 0..8 {
                for column in 0..8 {
                    if let Some(value) = record.source_pixel(column, row) {
                        if value != 0 {
                            taken[(usize::from(record.palette) + usize::from(value)).min(255)] =
                                true;
                        }
                    }
                }
            }
        }
        let mut free = (0..256).filter(|&entry| !taken[entry]);

        // Each line's colour, a line without one taking the line above's (or the first).
        let mut by_line = [None; 240];
        for &(scanline, color) in &sky {
            if let Some(slot) = by_line.get_mut(usize::from(scanline)) {
                *slot = Some(color);
            }
        }
        let mut placed: Vec<([u8; 3], usize)> = Vec::new();
        let mut entry_of_line = [0usize; 240];
        let mut current = first;
        let last_line = frame.height.min(239) as usize;
        for line in 1..=last_line {
            if let Some(color) = by_line[line] {
                current = color;
            }
            let entry = if let Some(&(_, entry)) = placed.iter().find(|(c, _)| *c == current) {
                entry
            } else if let Some(entry) = free.next() {
                let [r, g, b] = current;
                input.colors[entry] = Rgb24::new(r, g, b);
                placed.push((current, entry));
                entry
            } else {
                // No free entry left: the closest colour already placed.
                placed
                    .iter()
                    .min_by_key(|(c, _)| distance(*c, current))
                    .map_or(0, |&(_, entry)| entry)
            };
            entry_of_line[line] = entry;
        }
        if placed.is_empty() {
            return;
        }

        // Tiles cover screen lines 0 up, as the background tiles do (scanline 1 is line 0).
        let mut records = Vec::new();
        for row in 0..last_line.div_ceil(8) {
            let mut builder = Builder::default();
            for r in 0..8 {
                let entry = entry_of_line.get(row * 8 + r + 1).copied().unwrap_or(0);
                for (plane, slot) in builder.planes.iter_mut().enumerate() {
                    if entry & (1 << plane) != 0 {
                        *slot |= 0xFF << (8 * r);
                    }
                }
            }
            for column in 0..32 {
                let index = records.len() as i32;
                records.push(builder.record(column * 8, (row * 8) as i16, index, true));
            }
        }
        input.layers.push(LayerInput {
            layer: BACKDROP,
            records,
        });
    }

    /// The background rows put together per layer, which layers were drawn, and whether
    /// sprites were on the main screen.
    #[allow(clippy::type_complexity)] // Three plain results.
    fn background_tiles(&self) -> ([BTreeMap<BackgroundKey, Builder>; 5], [bool; 5], bool) {
        let mut layers: [BTreeMap<BackgroundKey, Builder>; 5] = Default::default();
        let mut used = [false; 5];
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
            sprites_on |= (chunk.main_layers | chunk.sub_layers) & 0x10 != 0;
            if chunk.bg_mode == 7 {
                self.mode7_tiles(chunk, &mut layers, &mut used);
                continue;
            }
            let Some(rules) = layer_bpp(chunk.bg_mode) else {
                continue;
            };
            let y = line.scanline as i16 - 1;
            for (layer, rule) in rules.iter().enumerate() {
                let Some((bpp, base)) = *rule else { continue };
                // A layer only on the sub screen (blended in, as Super Mario World's hills) is
                // part of the scene too.
                if (chunk.main_layers | chunk.sub_layers) & (1 << layer) == 0 {
                    continue;
                }
                used[layer] = true;
                // The window hides a layer's pixels on the main screen only; a layer also on the
                // sub screen keeps them there, so it is left whole.
                let on_main_only = chunk.sub_layers & (1 << layer) == 0;
                let fine = (chunk.hscroll[layer] & 0x07) as i16;
                for (column, tile) in line.tiles[layer].iter().enumerate() {
                    let x = column as i16 * 8 - fine;
                    if x >= 256 {
                        continue;
                    }
                    let shown_row = (line.scanline.wrapping_add(tile.vscroll) & 0x07) as i16;
                    let flip_x = tile.tilemap & 0x4000 != 0;
                    let flip_y = tile.tilemap & 0x8000 != 0;
                    let source_row = if flip_y { 7 - shown_row } else { shown_row };
                    let palette_number = (tile.tilemap >> 10) & 0x07;
                    let palette = if bpp == 8 {
                        0
                    } else {
                        base + palette_number * (1 << bpp)
                    };
                    let key = (y - shown_row, x, tile.tilemap);
                    // Mode 1's background 3 with its priority bit: the tiles that have their own
                    // priority bit set go in front of everything, the others stay at the back.
                    let target = if layer == 2
                        && chunk.bg_mode == 1
                        && chunk.bg3_priority != 0
                        && tile.tilemap & 0x2000 != 0
                    {
                        used[usize::from(BG3_FRONT)] = true;
                        usize::from(BG3_FRONT)
                    } else {
                        layer
                    };
                    let order = layers[target].len();
                    let builder = layers[target].entry(key).or_insert_with(|| Builder {
                        flip_x,
                        flip_y,
                        palette: palette.min(255) as u8,
                        order,
                        snapshot: (bpp != 8).then_some(chunk.cgram_index),
                        ..Builder::default()
                    });
                    let keep = shown_pixels(x, flip_x, |screen_x| {
                        on_main_only
                            && chunk.window_masked[layer][usize::from(screen_x >> 3)]
                                & (1 << (screen_x & 7))
                                != 0
                    });
                    builder.add_row(&tile.chr, usize::from(bpp), source_row as u32, keep);
                }
            }
        }

        (layers, used, sprites_on)
    }

    /// Cuts a Mode 7 line into rows of 8 pixel screen tiles. Mode 7 turns and stretches its
    /// picture pixel by pixel, so its own tiles do not reach the screen whole; the screen
    /// squares it covers are the tiles the engine gets instead.
    fn mode7_tiles(
        &self,
        chunk: &RawChunk,
        layers: &mut [BTreeMap<BackgroundKey, Builder>; 5],
        used: &mut [bool; 5],
    ) {
        let y = chunk.scanline as i16 - 1;
        let shown_row = y & 0x07;
        for layer in 0..2u8 {
            if (chunk.main_layers | chunk.sub_layers) & (1 << layer) == 0 {
                continue;
            }
            let Some(colors) = self.mode7_line(chunk.scanline, layer) else {
                continue;
            };
            let target = usize::from(layer);
            let on_main_only = chunk.sub_layers & (1 << layer) == 0;
            for column in 0..32u16 {
                let x = column * 8;
                // Background 2 keeps its priority in bit 7.
                let mask = if layer == 1 { 0x7F } else { 0xFF };
                let mut chr = [0u16; 4];
                for i in 0..8u16 {
                    let color = u16::from(colors[usize::from(x + i)] & mask);
                    for plane in 0..8 {
                        if color & (1 << plane) != 0 {
                            chr[plane / 2] |= 1 << ((7 - i) + 8 * (plane as u16 % 2));
                        }
                    }
                }
                if chr == [0; 4] {
                    continue;
                }
                used[target] = true;
                let keep = shown_pixels(x as i16, false, |screen_x| {
                    on_main_only
                        && chunk.window_masked[target][usize::from(screen_x >> 3)]
                            & (1 << (screen_x & 7))
                            != 0
                });
                let order = layers[target].len();
                let key = (y - shown_row, x as i16, MODE7_TILE);
                let builder = layers[target].entry(key).or_insert_with(|| Builder {
                    order,
                    ..Builder::default()
                });
                builder.add_row(&chr, 8, shown_row as u32, keep);
            }
        }
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
                let chunk = self
                    .chunks
                    .iter()
                    .rev()
                    .find(|c| c.scanline == sprite.fetch_scanline + 1);
                let mode = chunk.map_or(1, |c| c.bg_mode);
                let builder = sprites.entry(key).or_insert_with(|| Builder {
                    flip_x: sprite.horizontal_mirror != 0,
                    flip_y,
                    hidden: behind_layer_1(mode, sprite.priority),
                    palette: 128 + sprite.palette * 16,
                    order,
                    snapshot: chunk.map(|c| c.cgram_index),
                    ..Builder::default()
                });
                let keep = shown_pixels(sprite.x, sprite.horizontal_mirror != 0, |screen_x| {
                    chunk.is_some_and(|c| {
                        c.sub_layers & 0x10 == 0
                            && c.window_masked[4][usize::from(screen_x >> 3)]
                                & (1 << (screen_x & 7))
                                != 0
                    })
                });
                builder.add_row(&sprite.chr, 4, u32::from(source_row), keep);
            }
        }

        sprites
    }
}

#[cfg(test)]
mod tests {
    use super::{behind_layer_1, shown_pixels, FLOOR_MAX_DISTANCE};
    use crate::tap::{RawMode7Line, TileCapture};

    fn mode7_line(scanline: u16, a: i16) -> RawMode7Line {
        RawMode7Line {
            scanline,
            layer: 0,
            padding: 0,
            matrix: [a, 0, 0, a],
            color: [0; 256],
        }
    }

    #[test]
    fn a_floor_goes_back_as_its_scale_grows() {
        // A road: each line up covers more of the map than the one below it.
        let lines = (101..=200)
            .map(|s| mode7_line(s, (300 - s) as i16))
            .collect();
        let floor = TileCapture::with_mode7(lines).floor_distances();
        assert_eq!(floor[50], None);
        // Line 199 (scanline 200) is the nearest, scale 100; line 149 has scale 150.
        assert!((floor[199].unwrap() - 1.0).abs() < 1e-4);
        assert!((floor[149].unwrap() - 1.5).abs() < 1e-4);
        // Lines by the horizon are kept within reach.
        assert!(floor[100].unwrap() <= FLOOR_MAX_DISTANCE);
    }

    #[test]
    fn a_map_seen_from_above_stays_flat() {
        let lines = (1..=100).map(|s| mode7_line(s, 256)).collect();
        let floor = TileCapture::with_mode7(lines).floor_distances();
        assert!(floor.iter().flatten().all(|&far| (far - 1.0).abs() < 1e-6));
    }

    #[test]
    fn pixels_off_screen_or_behind_a_window_are_dropped() {
        // Three pixels hang off the left edge: the row's first three are gone.
        assert_eq!(shown_pixels(-3, false, |_| false), 0b0001_1111);
        // Mirrored, the row's first pixels are drawn on the right, so its last three go.
        assert_eq!(shown_pixels(-3, true, |_| false), 0b1111_1000);
        // A window over screen pixels 10 and 11.
        assert_eq!(shown_pixels(8, false, |x| x == 10 || x == 11), 0b1100_1111);
        assert_eq!(shown_pixels(252, false, |_| false), 0b1111_0000);
    }

    #[test]
    fn low_priority_sprites_go_behind_layer_1() {
        // Mode 1: priorities 0 and 1 behind, 2 and 3 in front.
        assert!(behind_layer_1(1, 0));
        assert!(behind_layer_1(1, 1));
        assert!(!behind_layer_1(1, 2));
        assert!(!behind_layer_1(1, 3));
        // Modes 2 to 4: only priority 0 behind.
        assert!(behind_layer_1(3, 0));
        assert!(!behind_layer_1(3, 1));
    }
}
