//! The 3D scene of a SNES frame: one conversion engine per background layer (the engine
//! knows one background, as the NES has one), whose tiles are put together front to back,
//! each layer moved back by its rank and grown so the picture still lines up seen from the
//! front. A Mode 7 floor is laid along its slope.
//!
//! Used by the `snes-preview` tool and by the app.

use std::collections::BTreeMap;

use zax_engine3d::draw_list::FrameDrawList;
use zax_engine3d::engine::ConversionEngine;
use zax_engine3d::error::EngineError;
use zax_engine3d::frame::ShapeId;
use zax_engine3d::vectors::{Mat4, Vec3};
use zax_profiles::profile::GameProfile;
use zax_profiles::values::Vec4F;

use crate::engine_input::{EngineInput, BACKDROP, BG3_FRONT};

/// How far back each background layer sits behind the one in front of it, in scene units
/// (pixels). Larger than the deepest object one engine makes (its layers L1 to L4 reach 36
/// plus the object's own depth), so the layers never cross.
pub const LAYER_STEP: f32 = 16.0;

/// The edges of the picture the engines leave out, left, bottom, right and top, as the engine
/// counts them (`ConversionSettings::clipping`, a 256 by 240 picture). The NES app hides 8
/// lines at the top and 8 columns on the right, which the SNES shows; the SNES picture has
/// 224 lines, so the 16 below it are left out.
pub const SNES_CLIPPING: Vec4F = Vec4F::new(0.1, 24.1, 0.1, -7.9);

/// The scene's clipping: the renderer cuts everything outside this rectangle, and the
/// camera aims at its middle. The layers further back are grown and reach past the
/// picture's edges, so the rectangle is the picture with a wide border all round; its middle
/// is the picture's ([`PICTURE_CENTER`]).
pub const SCENE_CLIPPING: Vec4F = Vec4F::new(-400.0, -400.0, -400.0, -432.0);

/// The middle of the 256 by 224 picture in scene coordinates (the engine puts screen line
/// `y` at `248 - y`).
pub const PICTURE_CENTER: Vec3 = Vec3::new(128.0, 136.0, 0.0);

/// How many tile images one engine may hold before it starts afresh: pictures that change
/// every frame (Mode 7, Super FX) bring new tile images all the time, and the engine keeps
/// every one it meets.
pub const IMAGE_LIMIT: usize = 3_000;

/// Sets up `engine` for one SNES layer: a profile with no patterns and the SNES clipping.
///
/// # Errors
///
/// The engine refused the empty profile.
pub fn prepare_engine(engine: &mut ConversionEngine) -> Result<(), EngineError> {
    engine.use_geometry();
    engine.load_profile(&GameProfile::create_empty(), true)?;
    engine.two_dimensional_mode = false;
    engine.settings_mut().clipping = SNES_CLIPPING;
    Ok(())
}

/// Converts one layer of `input` with `engine` (see [`LayerEngines::convert`]).
///
/// # Errors
///
/// The engine refused the empty profile when it was started afresh.
pub fn convert_layer(
    engine: &mut ConversionEngine,
    input: &EngineInput,
    layer: usize,
    counter: i32,
) -> Result<(), EngineError> {
    let Some(layer) = input.layers.get(layer) else {
        return Ok(());
    };
    if engine.registries_mut().1.count() > IMAGE_LIMIT {
        engine.unload();
        engine.load_profile(&GameProfile::create_empty(), true)?;
        engine.settings_mut().clipping = SNES_CLIPPING;
    }
    // The front part of mode 1's background 3 holds the game's text or status bar, and the
    // sky is a painted wall: Smart 3D keeps both as flat panels.
    engine.settings_mut().flat_layer = layer.layer == BG3_FRONT || layer.layer == BACKDROP;
    engine.convert(&input.batch(layer), counter);
    engine.publish();
    Ok(())
}

/// The engines of a frame's layers, indexed by `LayerInput::layer` (background layers 0 to 3,
/// [`BG3_FRONT`] and [`BACKDROP`]). An owner that keeps an engine of its own for a layer (the
/// app's, which runs the profile's scripts, for layer 0) leaves that one out.
#[derive(Default)]
pub struct LayerEngines {
    engines: [Option<ConversionEngine>; 6],
}

impl LayerEngines {
    /// No engines yet; each is made the first time its layer shows up.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Converts this frame's layers, creating an engine the first time a layer shows up.
    /// Layers in `skip` are left to their owner.
    ///
    /// # Errors
    ///
    /// An engine refused the empty profile.
    pub fn convert(
        &mut self,
        input: &EngineInput,
        counter: i32,
        skip: &[u8],
    ) -> Result<(), EngineError> {
        for (index, layer) in input.layers.iter().enumerate() {
            if skip.contains(&layer.layer) {
                continue;
            }
            let slot = &mut self.engines[usize::from(layer.layer)];
            if slot.is_none() {
                let mut engine = ConversionEngine::new();
                prepare_engine(&mut engine)?;
                *slot = Some(engine);
            }
            if let Some(engine) = slot.as_mut() {
                convert_layer(engine, input, index, counter)?;
            }
        }
        Ok(())
    }

    /// Drops the converted frames of every engine (a reset, a savestate put back).
    pub fn clear_frames(&mut self) {
        for engine in self.engines.iter_mut().flatten() {
            engine.clear_frames();
        }
    }

    /// The scene's tiles: the frame's layers, front to back, each moved back by its rank
    /// and grown so that, seen from a camera `distance` away from the picture, it still covers
    /// the same part of the picture as on the console. `own` gives the tiles of the layers
    /// whose engine is kept elsewhere.
    pub fn draw_list(
        &mut self,
        input: &EngineInput,
        distance: f32,
        own: &[(u8, &FrameDrawList)],
    ) -> FrameDrawList {
        let mut merged: Option<FrameDrawList> = None;
        let center = PICTURE_CENTER;
        for (rank, layer) in front_to_back(input).into_iter().enumerate() {
            let list = if let Some(&(_, list)) = own.iter().find(|(l, _)| *l == layer) {
                list
            } else if let Some(engine) = self.engines[usize::from(layer)].as_mut() {
                engine.build_draw_list()
            } else {
                continue;
            };
            #[allow(clippy::cast_precision_loss)] // At most 6 layers.
            let depth = rank as f32 * LAYER_STEP;
            let scale = (distance + depth) / distance.max(1.0);
            let merged = merged.get_or_insert_with(|| {
                // The list's palette, counter and time; the tiles are added below.
                let mut empty = list.clone();
                empty.clear_tiles();
                empty
            });
            if layer <= 1 && has_floor(input) {
                place_on_floor(merged, list, input, depth, center, distance);
            } else {
                merged.append_moved_scaled(list, depth, center, scale);
            }
        }
        merged.unwrap_or_default()
    }
}

/// The scene height of the top of screen line 0 (the engine puts line `y` at `248 - y`).
const SCENE_TOP: f32 = 248.0;

/// Whether a Mode 7 floor of the frame goes into the distance.
fn has_floor(input: &EngineInput) -> bool {
    input.floor.iter().flatten().any(|&far| far > 1.0)
}

/// Adds a Mode 7 layer's tiles laid along its floor: each line of the floor as far back as
/// the game draws it (`EngineInput::floor`), grown so it still covers the same part of the
/// picture from the front. A sprite stands where its lowest line meets the floor, whole.
fn place_on_floor(
    merged: &mut FrameDrawList,
    list: &FrameDrawList,
    input: &EngineInput,
    depth: f32,
    center: Vec3,
    distance: f32,
) {
    let mut feet: BTreeMap<ShapeId, i32> = BTreeMap::new();
    for tile in list.tiles().iter().filter(|t| !t.tile.background()) {
        let bottom = feet.entry(tile.instance.shape).or_insert(i32::MIN);
        *bottom = (*bottom).max(tile.tile.y() + 7);
    }
    let floor_at = |line: i32| -> f32 {
        usize::try_from(line)
            .ok()
            .and_then(|line| input.floor.get(line).copied().flatten())
            .unwrap_or(1.0)
    };
    // Where a screen line ends up: its depth and its scene height once grown.
    let place = |line: i32| {
        let far = floor_at(line);
        let depth = depth + distance * (far - 1.0);
        #[allow(clippy::cast_precision_loss)] // A screen line.
        let height = center.y + ((SCENE_TOP - line as f32) - center.y) * far;
        (depth, far, height)
    };
    merged.append_placed(list, |tile| {
        if !tile.tile.background() {
            // Sprites stand up, where their lowest line meets the floor.
            let feet_line = feet
                .get(&tile.instance.shape)
                .copied()
                .unwrap_or(tile.tile.y() + 7);
            let (depth, far, _) = place(feet_line);
            return FrameDrawList::moved_and_scaled(depth, center, far);
        }
        // A floor tile is laid along the slope from its top line to its bottom one, each
        // edge where that line goes, so the tiles of a floor meet without gaps.
        let (_, far, _) = place(tile.tile.y() + 4);
        let (top_depth, _, top_height) = place(tile.tile.y());
        let (bottom_depth, _, bottom_height) = place(tile.tile.y() + 8);
        let rise = top_height - bottom_height;
        let slope = if rise.abs() > 0.01 {
            (top_depth - bottom_depth) / rise
        } else {
            0.0
        };
        let middle = tile.tile_center;
        let placed_middle = Vec3::new(
            center.x + (middle.x - center.x) * far,
            f32::midpoint(top_height, bottom_height),
            middle.z + f32::midpoint(top_depth, bottom_depth),
        );
        Mat4::translation(placed_middle)
            * Mat4::shear_z_by_y(slope)
            * Mat4::scale_matrix(Vec3::new(far, rise / 8.0, 1.0))
            * Mat4::translation(Vec3::new(-middle.x, -middle.y, -middle.z))
    });
}

/// The frame's layers from the front one to the back one, as the SNES puts them: by the
/// layer's priority in the mode (Mesen's `RenderMode0` to `RenderMode4`, the higher of a
/// layer's two priorities), with the front part of mode 1's background 3 ([`BG3_FRONT`])
/// before everything.
#[must_use]
pub fn front_to_back(input: &EngineInput) -> Vec<u8> {
    // The sky ([`BACKDROP`]) is behind everything.
    let priorities: [u8; 6] = match input.bg_mode {
        0 => [11, 10, 5, 4, 0, 0],
        1 => [9, 8, 3, 1, 12, 0],
        _ => [7, 5, 2, 1, 0, 0],
    };
    let mut layers: Vec<u8> = input.layers.iter().map(|l| l.layer).collect();
    layers.sort_by_key(|&layer| std::cmp::Reverse(priorities[usize::from(layer)]));
    layers
}
