//! Boots a SNES ROM, runs it for a number of frames and saves pictures along the way.
//!
//! ```text
//! snes-boot <rom> [--frames N] [--shots 60,300,...] [--press SCRIPT] [--out DIR] [--name NAME]
//!           [--firmware DIR] [--check-tiles]
//! ```
//!
//! `--check-tiles` turns on the tile tap. At each shot it also writes `NAME-fNNNNN-tiles.png`:
//! the PPU's picture, the picture rebuilt from the recorded tiles, and the pixels that differ
//! in red, side by side. It prints `tiles <frame> checked <n> mismatched <n> skipped <n>
//! records <n>`. Skipped pixels are lines in modes 5, 6 and 7, not rebuilt yet.
//!
//! `--firmware` names a folder of chip firmware files under Mesen's names (`dsp1.rom`,
//! `st018.rom` and so on). Games with those chips, like Super Mario Kart, need them.
//!
//! For each shot frame it writes `DIR/NAME-fNNNNN.png` and prints `shot <frame> <width>x<height>
//! <hash>`. The defaults are 600 frames, a shot at the last frame, the current folder and the
//! ROM's file name.

use std::env;
use std::fs::{self, File};
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use zax_snes::{Frame, PressScript, Rebuild, Snes};

struct Options {
    rom: PathBuf,
    frames: u32,
    shots: Vec<u32>,
    press: PressScript,
    out: PathBuf,
    name: String,
    firmware: Option<PathBuf>,
    check_tiles: bool,
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("snes-boot: {message}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let options = parse(env::args().skip(1).collect())?;
    fs::create_dir_all(&options.out).map_err(|e| e.to_string())?;
    let home = env::temp_dir().join(format!("xsnes3d-boot-{}", std::process::id()));
    fs::create_dir_all(&home).map_err(|e| e.to_string())?;
    if let Some(firmware) = &options.firmware {
        copy_firmware(firmware, &home.join("Firmware"))?;
    }

    let mut snes = Snes::load(&options.rom, &home).map_err(|e| e.to_string())?;
    if options.check_tiles {
        snes.set_tile_tap(true).map_err(|e| e.to_string())?;
    }
    let mut mismatched_total = 0u64;
    for counter in 0..options.frames {
        snes.run_frame(options.press.at(counter))
            .map_err(|e| e.to_string())?;
        let done = counter + 1;
        if options.shots.contains(&done) {
            let frame = snes
                .frame()
                .ok_or_else(|| format!("no picture after frame {done}"))?;
            let path = options.out.join(format!("{}-f{done:05}.png", options.name));
            write_png(&path, &frame)?;
            println!(
                "shot {done} {}x{} {:016x} {}",
                frame.width,
                frame.height,
                frame.hash(),
                path.display()
            );
            if let Some(capture) = snes.tile_capture() {
                let rebuild = capture.rebuild();
                let path = options
                    .out
                    .join(format!("{}-f{done:05}-tiles.png", options.name));
                write_three_way(&path, &rebuild)?;
                println!(
                    "tiles {done} checked {} mismatched {} skipped {} records {}",
                    rebuild.checked,
                    rebuild.mismatched,
                    rebuild.skipped,
                    capture.records().len()
                );
                mismatched_total += u64::from(rebuild.mismatched);
            }
        }
    }
    drop(snes);
    // Mesen may have written save files; they are not needed.
    let _ = fs::remove_dir_all(&home);
    if mismatched_total > 0 {
        return Err(format!(
            "{mismatched_total} pixels rebuilt from the tiles differ from the picture"
        ));
    }
    Ok(())
}

fn parse(args: Vec<String>) -> Result<Options, String> {
    let mut rom = None;
    let mut frames = 600;
    let mut shots = Vec::new();
    let mut press = PressScript::default();
    let mut out = PathBuf::from(".");
    let mut name = None;
    let mut firmware = None;
    let mut check_tiles = false;
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        let mut value = |flag: &str| args.next().ok_or_else(|| format!("{flag} needs a value"));
        match arg.as_str() {
            "--frames" => {
                frames = value("--frames")?
                    .parse()
                    .map_err(|_| "--frames takes a number".to_string())?;
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
            "--out" => out = PathBuf::from(value("--out")?),
            "--name" => name = Some(value("--name")?),
            "--firmware" => firmware = Some(PathBuf::from(value("--firmware")?)),
            "--check-tiles" => check_tiles = true,
            flag if flag.starts_with("--") => return Err(format!("unknown option {flag}")),
            path => rom = Some(PathBuf::from(path)),
        }
    }
    let rom = rom.ok_or(
        "usage: snes-boot <rom> [--frames N] [--shots A,B] [--press SCRIPT] [--out DIR] [--name NAME] [--firmware DIR] [--check-tiles]",
    )?;
    if frames == 0 {
        return Err("--frames must be at least 1".into());
    }
    if shots.is_empty() {
        shots.push(frames);
    }
    if let Some(late) = shots.iter().find(|&&shot| shot == 0 || shot > frames) {
        return Err(format!("shot {late} is outside frames 1 to {frames}"));
    }
    let name = name.unwrap_or_else(|| file_stem(&rom));
    Ok(Options {
        rom,
        frames,
        shots,
        press,
        out,
        name,
        firmware,
        check_tiles,
    })
}

/// Copies the firmware files into the Firmware folder Mesen reads under its home folder.
fn copy_firmware(from: &Path, to: &Path) -> Result<(), String> {
    fs::create_dir_all(to).map_err(|e| e.to_string())?;
    let entries = fs::read_dir(from).map_err(|e| format!("{}: {e}", from.display()))?;
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if path.is_file() {
            fs::copy(&path, to.join(entry.file_name())).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

fn file_stem(path: &Path) -> String {
    path.file_stem().map_or_else(
        || "snes".to_string(),
        |stem| stem.to_string_lossy().into_owned(),
    )
}

/// The PPU's picture, the rebuilt one and the differences (red, over a dimmed picture), side
/// by side with 4-pixel gaps.
fn write_three_way(path: &Path, rebuild: &Rebuild) -> Result<(), String> {
    let (width, height) = (rebuild.width as usize, rebuild.height as usize);
    let gap = 4;
    let total = width * 3 + gap * 2;
    let mut rgb = vec![40u8; total * height * 3];
    for y in 0..height {
        for x in 0..width {
            let at = (y * width + x) * 3;
            let reference = &rebuild.reference[at..at + 3];
            let rebuilt = &rebuild.rebuilt[at..at + 3];
            let difference = if reference == rebuilt {
                [reference[0] / 3, reference[1] / 3, reference[2] / 3]
            } else {
                [255, 0, 0]
            };
            for (panel, colour) in [reference, rebuilt, &difference[..]].iter().enumerate() {
                let out = (y * total + panel * (width + gap) + x) * 3;
                rgb[out..out + 3].copy_from_slice(colour);
            }
        }
    }
    let frame = Frame {
        width: u32::try_from(total).map_err(|e| e.to_string())?,
        height: rebuild.height,
        rgb,
    };
    write_png(path, &frame)
}

fn write_png(path: &Path, frame: &Frame) -> Result<(), String> {
    let file = File::create(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut encoder = png::Encoder::new(BufWriter::new(file), frame.width, frame.height);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().map_err(|e| e.to_string())?;
    writer
        .write_image_data(&frame.rgb)
        .map_err(|e| e.to_string())
}
