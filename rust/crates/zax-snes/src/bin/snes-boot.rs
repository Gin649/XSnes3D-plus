//! Boots a SNES ROM, runs it for a number of frames and saves pictures along the way.
//!
//! ```text
//! snes-boot <rom> [--frames N] [--shots 60,300,...] [--press SCRIPT] [--out DIR] [--name NAME]
//! ```
//!
//! For each shot frame it writes `DIR/NAME-fNNNNN.png` and prints `shot <frame> <width>x<height>
//! <hash>`. The defaults are 600 frames, a shot at the last frame, the current folder and the
//! ROM's file name.

use std::env;
use std::fs::{self, File};
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use zax_snes::{Frame, PressScript, Snes};

struct Options {
    rom: PathBuf,
    frames: u32,
    shots: Vec<u32>,
    press: PressScript,
    out: PathBuf,
    name: String,
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

    let mut snes = Snes::load(&options.rom, &home).map_err(|e| e.to_string())?;
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
        }
    }
    drop(snes);
    // Mesen may have written save files; they are not needed.
    let _ = fs::remove_dir_all(&home);
    Ok(())
}

fn parse(args: Vec<String>) -> Result<Options, String> {
    let mut rom = None;
    let mut frames = 600;
    let mut shots = Vec::new();
    let mut press = PressScript::default();
    let mut out = PathBuf::from(".");
    let mut name = None;
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
            flag if flag.starts_with("--") => return Err(format!("unknown option {flag}")),
            path => rom = Some(PathBuf::from(path)),
        }
    }
    let rom = rom.ok_or("usage: snes-boot <rom> [--frames N] [--shots A,B] [--press SCRIPT] [--out DIR] [--name NAME]")?;
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
    })
}

fn file_stem(path: &Path) -> String {
    path.file_stem().map_or_else(
        || "snes".to_string(),
        |stem| stem.to_string_lossy().into_owned(),
    )
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
