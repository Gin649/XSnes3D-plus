//! Plays a SNES ROM and draws pictures of it: the flat picture, the plain 3D conversion and the
//! Smart 3D one, side by side, for each camera angle asked for.
//!
//! ```text
//! snes-preview <rom> [--frames N] [--shots 600,1200] [--press SCRIPT] [--firmware DIR]
//!              [--angles 0.81,0.81;22,-30] [--out DIR] [--name NAME] [--size 512x448]
//! ```
//!
//! Each background layer of the SNES picture goes to its own 3D engine (the engine knows one
//! background, as the NES has one); sprites go with layer 1. Two sets of engines run side by
//! side: one with Smart 3D off (plain 3D) and one with it on. At each shot, each set's layers
//! are drawn as one scene, the layers further back by [`LAYER_STEP`] per rank, in the order
//! the SNES itself layers them. For each shot and angle it writes
//! `NAME-fNNNNN-aK.png` (K counts the angles from 0) and prints what it drew.

use std::env;
use std::fs::{self, File};
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use zax_engine3d::draw_list::FrameDrawList;
use zax_engine3d::engine::ConversionEngine;
use zax_engine3d::palette::Rgb24;
use zax_engine3d::vectors::Vec3;
use zax_profiles::profile::GameProfile;
use zax_profiles::values::Vec4F;
use zax_render::geometry::VisibleRect;
use zax_render::numerics::Vector3;
use zax_render::offscreen::{ContextApi, OffscreenContext};
use zax_render::scene::{SceneFrame, SceneState};
use zax_render::SceneRenderer;
use zax_settings::catalogue::SettingId;
use zax_settings::model::SettingsModel;
use zax_snes::{EngineInput, PressScript, Snes, BACKDROP, BG3_FRONT};

/// How far back each background layer sits behind the one in front of it, in scene units
/// (pixels). Larger than the deepest object one engine makes (its layers L1 to L4 reach 36
/// plus the object's own depth), so the layers never cross.
const LAYER_STEP: f32 = 16.0;

/// How much closer than the NES app's default the camera comes, so the SNES picture fills more
/// of the view.
const CAMERA_CLOSER: f32 = 0.85;

/// The edges of the picture the engines leave out, left, bottom, right and top, as the engine
/// counts them (`ConversionSettings::clipping`, a 256 by 240 picture). The NES app hides 8
/// lines at the top and 8 columns on the right, which the SNES shows; the SNES picture has
/// 224 lines, so the 16 below it are left out.
const SNES_CLIPPING: Vec4F = Vec4F::new(0.1, 24.1, 0.1, -7.9);

/// The scene's clipping: the renderer cuts everything outside this rectangle, and the
/// camera aims at its middle. The layers further back are grown (see `EngineSet::draw_list`)
/// and reach past the picture's edges, so the rectangle is the picture with a wide border
/// all round; its middle is the picture's ([`picture_center`]).
const SCENE_CLIPPING: Vec4F = Vec4F::new(-64.0, -40.0, -64.0, -72.0);

/// The camera angles of the NES preview: almost head on, and from above and to the left.
const DEFAULT_ANGLES: &str = "0.81,0.81;22,-30";

struct Options {
    rom: PathBuf,
    frames: u32,
    shots: Vec<u32>,
    press: PressScript,
    firmware: Option<PathBuf>,
    angles: Vec<(f32, f32)>,
    out: PathBuf,
    name: String,
    size: (i32, i32),
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("snes-preview: {message}");
            ExitCode::FAILURE
        }
    }
}

/// How many tile images one engine may hold before it starts afresh.
const IMAGE_LIMIT: usize = 3_000;

/// One set of engines, one per background layer (indexed by layer 0 to 3), one for the front
/// part of mode 1's background 3 ([`BG3_FRONT`]) and one for the sky ([`BACKDROP`]).
struct EngineSet {
    smart: bool,
    engines: [Option<ConversionEngine>; 6],
}

impl EngineSet {
    fn new(smart: bool) -> Self {
        Self {
            smart,
            engines: [None, None, None, None, None, None],
        }
    }

    /// Converts this frame's layers, creating an engine the first time a layer shows up.
    fn convert(&mut self, input: &EngineInput, counter: i32) -> Result<(), String> {
        // Smart 3D is a switch shared by every engine; each set sets it before converting.
        zax_engine3d::smart3d::set_enabled(self.smart);
        for layer in &input.layers {
            let slot = &mut self.engines[usize::from(layer.layer)];
            if slot.is_none() {
                let mut engine = ConversionEngine::new();
                engine.use_geometry();
                engine
                    .load_profile(&GameProfile::create_empty(), true)
                    .map_err(|error| error.to_string())?;
                engine.two_dimensional_mode = false;
                engine.settings_mut().clipping = SNES_CLIPPING;
                *slot = Some(engine);
            }
            if let Some(engine) = slot.as_mut() {
                // Pictures that change every frame (Mode 7, Super FX) bring new tile images
                // all the time; the engine keeps every one it meets, so start it afresh when
                // it holds too many.
                if engine.registries_mut().1.count() > IMAGE_LIMIT {
                    engine.unload();
                    engine
                        .load_profile(&GameProfile::create_empty(), true)
                        .map_err(|error| error.to_string())?;
                    engine.settings_mut().clipping = SNES_CLIPPING;
                }
                // The front part of mode 1's background 3 holds the game's text or status bar,
                // and the sky is a painted wall: Smart 3D keeps both as flat panels.
                engine.settings_mut().flat_layer =
                    layer.layer == BG3_FRONT || layer.layer == BACKDROP;
                engine.convert(&input.batch(layer), counter);
                engine.publish();
            }
        }
        Ok(())
    }

    /// The scene's tiles: the frame's layers, front to back, each moved back by its rank
    /// and grown so that, seen from a camera `distance` away from the picture, it still covers
    /// the same part of the picture as on the console.
    fn draw_list(&mut self, input: &EngineInput, distance: f32) -> FrameDrawList {
        let mut merged: Option<FrameDrawList> = None;
        let center = picture_center();
        for (rank, layer) in front_to_back(input).into_iter().enumerate() {
            let Some(engine) = self.engines[usize::from(layer)].as_mut() else {
                continue;
            };
            let list = engine.build_draw_list();
            #[allow(clippy::cast_precision_loss)] // At most 6 layers.
            let depth = rank as f32 * LAYER_STEP;
            let scale = (distance + depth) / distance.max(1.0);
            let merged = merged.get_or_insert_with(|| {
                // The list's palette, counter and time; the tiles are added below.
                let mut empty = list.clone();
                empty.clear_tiles();
                empty
            });
            merged.append_moved_scaled(list, depth, center, scale);
        }
        merged.unwrap_or_default()
    }
}

/// The frame's layers from the front one to the back one, as the SNES puts them: by the
/// layer's priority in the mode (Mesen's `RenderMode0` to `RenderMode4`, the higher of a
/// layer's two priorities), with the front part of mode 1's background 3 ([`BG3_FRONT`])
/// before everything.
fn front_to_back(input: &EngineInput) -> Vec<u8> {
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

fn run() -> Result<(), String> {
    let options = parse(env::args().skip(1).collect())?;
    fs::create_dir_all(&options.out).map_err(|e| e.to_string())?;
    let home = env::temp_dir().join(format!("xsnes3d-preview-{}", std::process::id()));
    fs::create_dir_all(home.join("Firmware")).map_err(|e| e.to_string())?;
    if let Some(firmware) = &options.firmware {
        for entry in fs::read_dir(firmware).map_err(|e| e.to_string())?.flatten() {
            if entry.path().is_file() {
                fs::copy(entry.path(), home.join("Firmware").join(entry.file_name()))
                    .map_err(|e| e.to_string())?;
            }
        }
    }

    let (width, height) = options.size;
    let context =
        OffscreenContext::new(width, height, ContextApi::DesktopCore).map_err(|e| e.to_string())?;
    let mut renderer = SceneRenderer::new(context.gl(), None).map_err(|e| e.to_string())?;

    let mut snes = Snes::load(&options.rom, &home).map_err(|e| e.to_string())?;
    snes.set_tile_tap(true).map_err(|e| e.to_string())?;
    let mut plain = EngineSet::new(false);
    let mut smart = EngineSet::new(true);

    for counter in 0..options.frames {
        snes.run_frame(options.press.at(counter))
            .map_err(|e| e.to_string())?;
        let Some(capture) = snes.tile_capture() else {
            continue;
        };
        let mut input = capture.engine_input();
        if let Some(frame) = snes.frame() {
            capture.add_backdrop(&mut input, &frame);
        }
        let counter_i32 = i32::try_from(counter).unwrap_or(i32::MAX);
        plain.convert(&input, counter_i32)?;
        smart.convert(&input, counter_i32)?;

        let done = counter + 1;
        if !options.shots.contains(&done) {
            continue;
        }
        let flat = snes
            .frame()
            .ok_or_else(|| format!("no picture after frame {done}"))?;
        // The sky as the finished picture shows it (games often blend it in).
        let sky = capture.sky_color(&flat);
        let plain_list = plain.draw_list(&input, camera_distance());
        let smart_list = smart.draw_list(&input, camera_distance());
        for (index, &(rx, ry)) in options.angles.iter().enumerate() {
            let mut panels = Vec::new();
            for list in [&plain_list, &smart_list] {
                let mut scene = scene_state(rx, ry, list);
                if let Some([r, g, b]) = sky {
                    scene.nes_background_color = Rgb24::new(r, g, b);
                }
                renderer
                    .render(&context.target(), &scene, &SceneFrame::with_draw_list(list))
                    .map_err(|e| e.to_string())?;
                context.finish();
                panels.push(context.read_pixels());
            }
            let path = options
                .out
                .join(format!("{}-f{done:05}-a{index}.png", options.name));
            write_three_way(&path, &flat, &panels, options.size)?;
            println!(
                "picture {done} angle {rx},{ry} layers {:?} tiles plain {} smart {} {}",
                input.layers.iter().map(|l| l.layer).collect::<Vec<_>>(),
                plain_list.tiles().len(),
                smart_list.tiles().len(),
                path.display()
            );
        }
    }
    drop(snes);
    let _ = fs::remove_dir_all(&home);
    Ok(())
}

/// The camera's distance from the picture: the NES app's default, brought closer.
fn camera_distance() -> f32 {
    let fov = SettingsModel::new().get(SettingId::CameraFov).as_float();
    zax_render::geometry::reset_distance(fov) * CAMERA_CLOSER
}

/// The middle of the visible picture, in scene coordinates, where the camera aims.
fn picture_center() -> Vec3 {
    let center = VisibleRect::from_clipping(SNES_CLIPPING).center();
    Vec3::new(center.x, center.y, 0.0)
}

/// The scene settings the NES app starts with, the camera at `rx`, `ry` degrees.
fn scene_state(rx: f32, ry: f32, list: &FrameDrawList) -> SceneState {
    let model = SettingsModel::new();
    let mut scene = SceneState::default();
    let fov = model.get(SettingId::CameraFov).as_float();
    scene.settings.fov = fov;
    scene.camera.set_fov(fov);
    scene.camera.smooth_time = model.get(SettingId::CameraSmoothTime).as_float();
    scene.camera.local_camera = model.get(SettingId::CameraLocalCam).as_bool();
    scene.settings.shadows = model.get(SettingId::RenderShadow).as_bool();
    scene.use_nes_background_color = true;
    scene.nes_background_color = list.palette()[0];
    scene.camera.current_mut().angles = Vector3::new(rx, ry, 0.0);
    // The NES app's distance leaves a wide border around the SNES picture; come closer.
    let distance = camera_distance();
    scene.camera.current_mut().set_distance(distance);
    scene.clipping = SCENE_CLIPPING;
    scene.camera.snap_to_setpoints();
    scene.game_running = true;
    scene
}

/// The flat picture (scaled to the panel size) and the two 3D panels, side by side with 4
/// pixel gaps. The panels are RGBA rows, top first.
fn write_three_way(
    path: &Path,
    flat: &zax_snes::Frame,
    panels: &[Vec<u8>],
    (width, height): (i32, i32),
) -> Result<(), String> {
    let (w, h) = (
        usize::try_from(width).map_err(|e| e.to_string())?,
        usize::try_from(height).map_err(|e| e.to_string())?,
    );
    let gap = 4;
    let total = w * 3 + gap * 2;
    let mut rgb = vec![40u8; total * h * 3];
    let (fw, fh) = (flat.width as usize, flat.height as usize);
    for y in 0..h {
        for x in 0..w {
            let out = (y * total + x) * 3;
            let (sx, sy) = (x * fw / w, y * fh / h);
            let at = (sy * fw + sx) * 3;
            rgb[out..out + 3].copy_from_slice(&flat.rgb[at..at + 3]);
            for (panel, pixels) in panels.iter().enumerate() {
                let at = (y * w + x) * 4;
                let out = (y * total + (panel + 1) * (w + gap) + x) * 3;
                if let Some(pixel) = pixels.get(at..at + 3) {
                    rgb[out..out + 3].copy_from_slice(pixel);
                }
            }
        }
    }
    let file = File::create(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut encoder = png::Encoder::new(
        BufWriter::new(file),
        u32::try_from(total).map_err(|e| e.to_string())?,
        u32::try_from(h).map_err(|e| e.to_string())?,
    );
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().map_err(|e| e.to_string())?;
    writer.write_image_data(&rgb).map_err(|e| e.to_string())
}

fn parse(args: Vec<String>) -> Result<Options, String> {
    let mut rom = None;
    let mut frames = 600;
    let mut shots = Vec::new();
    let mut press = PressScript::default();
    let mut firmware = None;
    let mut angles_text = DEFAULT_ANGLES.to_owned();
    let mut out = PathBuf::from(".");
    let mut name = None;
    let mut size = (512, 448);
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        let mut value = |flag: &str| args.next().ok_or_else(|| format!("{flag} needs a value"));
        match arg.as_str() {
            "--frames" => {
                frames = value("--frames")?
                    .parse()
                    .map_err(|_| "--frames takes a number".to_owned())?;
            }
            "--shots" => {
                for shot in value("--shots")?
                    .split(',')
                    .filter(|s| !s.trim().is_empty())
                {
                    shots.push(
                        shot.trim()
                            .parse()
                            .map_err(|_| format!("'{shot}' is not a frame number"))?,
                    );
                }
            }
            "--press" => {
                press = PressScript::parse(&value("--press")?).map_err(|e| e.to_string())?;
            }
            "--firmware" => firmware = Some(PathBuf::from(value("--firmware")?)),
            "--angles" => angles_text = value("--angles")?,
            "--out" => out = PathBuf::from(value("--out")?),
            "--name" => name = Some(value("--name")?),
            "--size" => {
                let text = value("--size")?;
                let (w, h) = text
                    .split_once('x')
                    .ok_or_else(|| format!("--size expects WxH, got '{text}'"))?;
                size = (
                    w.parse().map_err(|_| format!("bad width '{w}'"))?,
                    h.parse().map_err(|_| format!("bad height '{h}'"))?,
                );
            }
            flag if flag.starts_with("--") => return Err(format!("unknown option {flag}")),
            path => rom = Some(PathBuf::from(path)),
        }
    }
    let rom = rom.ok_or("usage: snes-preview <rom> [--frames N] [--shots A,B] [--press SCRIPT] [--firmware DIR] [--angles X,Y;X,Y] [--out DIR] [--name NAME] [--size WxH]")?;
    if shots.is_empty() {
        shots.push(frames);
    }
    frames = frames.max(shots.iter().copied().max().unwrap_or(frames));
    let mut angles = Vec::new();
    for pair in angles_text.split(';').filter(|p| !p.trim().is_empty()) {
        let (x, y) = pair
            .split_once(',')
            .ok_or_else(|| format!("--angles expects X,Y pairs, got '{pair}'"))?;
        angles.push((
            x.trim().parse().map_err(|_| format!("bad angle '{x}'"))?,
            y.trim().parse().map_err(|_| format!("bad angle '{y}'"))?,
        ));
    }
    let name = name.unwrap_or_else(|| {
        rom.file_stem()
            .map_or_else(|| "snes".to_owned(), |s| s.to_string_lossy().into_owned())
    });
    Ok(Options {
        rom,
        frames,
        shots,
        press,
        firmware,
        angles,
        out,
        name,
        size,
    })
}

#[cfg(test)]
mod tests {
    use super::{picture_center, SCENE_CLIPPING};
    use zax_render::geometry::VisibleRect;

    #[test]
    fn the_camera_aims_at_the_middle_of_the_picture() {
        // The renderer aims at the middle of the scene's rectangle; the layers are grown about
        // the picture's middle. Both must be the same point, or the layers drift apart.
        let scene = VisibleRect::from_clipping(SCENE_CLIPPING).center();
        let picture = picture_center();
        assert!((scene.x - picture.x).abs() < 0.01);
        assert!((scene.y - picture.y).abs() < 0.01);
    }
}
