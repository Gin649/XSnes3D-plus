//! Plays a SNES ROM and draws pictures of it: the flat picture, the plain 3D conversion and the
//! Smart 3D one, side by side, for each camera angle asked for.
//!
//! ```text
//! snes-preview <rom> [--frames N] [--shots 600,1200] [--press SCRIPT] [--firmware DIR]
//!              [--angles 0.81,0.81;22,-30] [--out DIR] [--name NAME] [--size 512x448]
//!              [--timing]
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
use std::time::{Duration, Instant};

use zax_engine3d::draw_list::FrameDrawList;
use zax_engine3d::palette::Rgb24;
use zax_render::numerics::Vector3;
use zax_render::offscreen::{ContextApi, OffscreenContext};
use zax_render::scene::{SceneFrame, SceneState};
use zax_render::SceneRenderer;
use zax_settings::catalogue::SettingId;
use zax_settings::model::SettingsModel;
use zax_snes::layers::{LayerEngines, SCENE_CLIPPING};
use zax_snes::{EngineInput, PressScript, Snes};

/// How much closer than the NES app's default the camera comes, so the SNES picture fills more
/// of the view.
const CAMERA_CLOSER: f32 = 0.85;

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
    /// Print how long each stage took per frame.
    timing: bool,
    /// A save state to start from (the app's `state.bin` of a reported moment, or Mesen's own).
    state: Option<PathBuf>,
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

/// Time spent per stage over the run, for `--timing`.
#[derive(Default)]
struct Timing {
    frames: u32,
    emulate: Duration,
    tiles: Duration,
    engines: Duration,
}

impl Timing {
    /// Prints the average milliseconds per frame of each stage.
    fn print(&self) {
        let per_frame =
            |total: Duration| total.as_secs_f64() * 1000.0 / f64::from(self.frames.max(1));
        println!(
            "timing over {} frames, ms per frame: emulation {:.2}, tiles {:.2}, Smart 3D engines {:.2}",
            self.frames,
            per_frame(self.emulate),
            per_frame(self.tiles),
            per_frame(self.engines),
        );
    }
}

/// One set of engines (plain 3D or Smart 3D).
struct EngineSet {
    smart: bool,
    engines: LayerEngines,
}

impl EngineSet {
    fn new(smart: bool) -> Self {
        Self {
            smart,
            engines: LayerEngines::new(),
        }
    }

    /// Converts this frame's layers.
    fn convert(&mut self, input: &EngineInput, counter: i32) -> Result<(), String> {
        // Smart 3D is a switch shared by every engine; each set sets it before converting.
        zax_engine3d::smart3d::set_enabled(self.smart);
        self.engines
            .convert(input, counter, &[])
            .map_err(|error| error.to_string())
    }

    /// The scene's tiles for a camera `distance` away.
    fn draw_list(&mut self, input: &EngineInput, distance: f32) -> FrameDrawList {
        self.engines.draw_list(input, distance, &[])
    }
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
    if let Some(path) = &options.state {
        let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        snes.run_frame(zax_snes::Buttons::default())
            .map_err(|e| e.to_string())?;
        snes.load_state(mesen_state(&bytes))
            .map_err(|e| e.to_string())?;
    }
    let mut plain = EngineSet::new(false);
    let mut smart = EngineSet::new(true);

    let mut timing = Timing::default();
    for counter in 0..options.frames {
        let started = Instant::now();
        snes.run_frame(options.press.at(counter))
            .map_err(|e| e.to_string())?;
        timing.emulate += started.elapsed();
        let started = Instant::now();
        let Some(capture) = snes.tile_capture() else {
            continue;
        };
        let mut input = capture.engine_input();
        if let Some(frame) = snes.frame() {
            let look = capture.look(&frame);
            capture.apply_finished_colors(&mut input, &look);
            capture.add_backdrop(&mut input, &frame, &look);
        }
        timing.tiles += started.elapsed();
        let counter_i32 = i32::try_from(counter).unwrap_or(i32::MAX);
        let started = Instant::now();
        smart.convert(&input, counter_i32)?;
        timing.engines += started.elapsed();
        timing.frames += 1;
        plain.convert(&input, counter_i32)?;

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
    if options.timing {
        timing.print();
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

/// Mesen's own state inside a state the app wrote: the app adds the ROM, its name, their
/// lengths and the frame count at the end (`SnesCore::save_state_to`). A moment's state carries
/// no ROM, so its last 20 bytes are the two lengths (0) and the frame count.
fn mesen_state(bytes: &[u8]) -> &[u8] {
    let Some(tail) = bytes.len().checked_sub(20) else {
        return bytes;
    };
    let lengths = &bytes[tail..tail + 12];
    if lengths.iter().all(|&byte| byte == 0) {
        &bytes[..tail]
    } else {
        bytes
    }
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
    let mut timing = false;
    let mut state = None;
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
            "--timing" => timing = true,
            "--state" => state = Some(PathBuf::from(value("--state")?)),
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
    let rom = rom.ok_or("usage: snes-preview <rom> [--frames N] [--shots A,B] [--press SCRIPT] [--firmware DIR] [--angles X,Y;X,Y] [--out DIR] [--name NAME] [--size WxH] [--timing] [--state FILE]")?;
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
        timing,
        state,
    })
}

#[cfg(test)]
mod tests {
    use zax_render::geometry::VisibleRect;
    use zax_snes::layers::{PICTURE_CENTER, SCENE_CLIPPING, SNES_CLIPPING};

    #[test]
    fn the_camera_aims_at_the_middle_of_the_picture() {
        // The renderer aims at the middle of the scene's rectangle; the layers are grown about
        // the picture's middle. Both must be the same point, or the layers drift apart.
        let scene = VisibleRect::from_clipping(SCENE_CLIPPING).center();
        let picture = VisibleRect::from_clipping(SNES_CLIPPING).center();
        for center in [scene, picture] {
            assert!((center.x - PICTURE_CENTER.x).abs() < 0.01);
            assert!((center.y - PICTURE_CENTER.y).abs() < 0.01);
        }
    }
}
