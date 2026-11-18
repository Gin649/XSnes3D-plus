//! The SNES core as the app sees it: `zax_snes::Snes` with what the app keeps beside it (the
//! frame's picture and 3D layers, the sound waiting for the audio stream, the ROM it loaded).
//!
//! The NES app talked to its own core here; the SNES runs Mesen 2 through `zax-snes`. A game
//! is loaded from the bytes the app read, through a copy in the core's home folder, since
//! Mesen loads files.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use zax_snes::{Buttons, EngineInput, Snes};

/// Frames a second of a NTSC SNES (21.477 MHz master clock, 357,366 clocks a frame).
pub const NTSC_FRAME_RATE: f64 = 60.098_8;

/// The sample rate Mesen mixes its sound at.
pub const SAMPLE_RATE: i32 = 48_000;

/// The most the sound's speed may be nudged to keep the audio stream's buffer steady (the
/// share of the rate, either way).
pub const MAX_RATIO_ADJUSTMENT: f64 = 0.005;

/// Sound kept for the audio stream at most, in stereo frames (half a second).
const RING_FRAMES: usize = 24_000;

/// The pads of both players in one word: player 1 in bits 0 to 11, player 2 in bits 16 to
/// 27, each in Mesen's order (A, B, X, Y, L, R, Select, Start, Up, Down, Left, Right).
pub type PadWord = u32;

/// What a ROM load gave.
#[derive(Debug, Clone, Default)]
pub struct RomLoadResult {
    /// Whether the game runs.
    pub success: bool,
    /// Why not, when it does not.
    pub error: Option<String>,
}

/// Sound made by the core and not played yet: stereo frames, left first, -1 to 1.
#[derive(Default)]
struct SampleRing {
    samples: std::collections::VecDeque<f32>,
}

fn lock(ring: &Mutex<SampleRing>) -> MutexGuard<'_, SampleRing> {
    ring.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The audio stream's end of the sound: it may live on another thread.
#[derive(Clone, Default)]
pub struct AudioReader {
    ring: Arc<Mutex<SampleRing>>,
}

impl AudioReader {
    /// Fills `destination` (stereo pairs when `stereo`, else mono) when that much sound is
    /// waiting; returns how many values were written: all of them, or 0 when not enough
    /// sound is there yet.
    pub fn read_samples(&self, destination: &mut [f32], stereo: bool) -> usize {
        let mut ring = lock(&self.ring);
        let frames = if stereo {
            destination.len() / 2
        } else {
            destination.len()
        };
        if frames == 0 || ring.samples.len() / 2 < frames {
            return 0;
        }
        for i in 0..frames {
            let left = ring.samples.pop_front().unwrap_or(0.0);
            let right = ring.samples.pop_front().unwrap_or(0.0);
            if stereo {
                destination[2 * i] = left;
                destination[2 * i + 1] = right;
            } else {
                destination[i] = f32::midpoint(left, right);
            }
        }
        if stereo {
            frames * 2
        } else {
            frames
        }
    }

    /// Stereo frames waiting.
    #[must_use]
    pub fn available_samples(&self) -> usize {
        lock(&self.ring).samples.len() / 2
    }

    fn push(&self, samples: &[i16]) {
        let mut ring = lock(&self.ring);
        ring.samples
            .extend(samples.iter().map(|&s| f32::from(s) / 32_768.0));
        // The oldest sound goes when the stream falls behind.
        let over = ring.samples.len().saturating_sub(RING_FRAMES * 2);
        ring.samples.drain(..over);
    }

    fn clear(&self) {
        lock(&self.ring).samples.clear();
    }
}

/// The SNES core of one game.
pub struct SnesCore {
    snes: Option<Snes>,
    home: PathBuf,
    image: Option<Vec<u8>>,
    frames: i64,
    input: Option<EngineInput>,
    picture: Option<zax_snes::Frame>,
    audio: AudioReader,
    sound: Vec<i16>,
}

impl Default for SnesCore {
    fn default() -> Self {
        Self::new()
    }
}

impl SnesCore {
    /// A core with no game.
    #[must_use]
    pub fn new() -> Self {
        Self {
            snes: None,
            home: default_home(),
            image: None,
            frames: 0,
            input: None,
            picture: None,
            audio: AudioReader::default(),
            sound: Vec::new(),
        }
    }

    /// Where Mesen keeps its files: battery saves in `Saves`, its settings, and the copy of
    /// the game it loads. Takes effect at the next [`SnesCore::load_rom`].
    pub fn set_home(&mut self, home: PathBuf) {
        self.home = home;
    }

    /// Loads a game from its file's bytes; `name` is the file's name.
    pub fn load_rom(&mut self, image: &[u8], name: &str) -> RomLoadResult {
        // Only one Mesen may run at a time: the game before goes first.
        self.snes = None;
        self.input = None;
        self.picture = None;
        self.audio.clear();
        let folder = self.home.join("Games");
        if let Err(error) = std::fs::create_dir_all(&folder) {
            return failure(format!("cannot make {}: {error}", folder.display()));
        }
        let path = folder.join(file_name(name));
        if let Err(error) = std::fs::write(&path, image) {
            return failure(format!("cannot write {}: {error}", path.display()));
        }
        match Snes::load(&path, &self.home) {
            Ok(mut snes) => {
                if let Err(error) = snes.set_tile_tap(true) {
                    return failure(error.to_string());
                }
                self.snes = Some(snes);
                self.image = Some(image.to_vec());
                self.frames = 0;
                RomLoadResult {
                    success: true,
                    error: None,
                }
            }
            Err(error) => failure(error.to_string()),
        }
    }

    /// Whether a game is loaded.
    #[must_use]
    pub const fn is_running(&self) -> bool {
        self.snes.is_some()
    }

    /// Frames run since the game was loaded.
    #[must_use]
    pub const fn frame_count(&self) -> i64 {
        self.frames
    }

    /// The bytes of the game file loaded.
    #[must_use]
    pub fn original_image(&self) -> Option<&[u8]> {
        self.image.as_deref()
    }

    /// Runs one frame with the pads of `word`; returns the frame count. Without a game it
    /// does nothing.
    pub fn run_frame(&mut self, word: PadWord) -> i64 {
        let Some(snes) = self.snes.as_mut() else {
            return self.frames;
        };
        #[allow(clippy::cast_possible_truncation)] // Twelve bits each.
        let (one, two) = (word as u16, (word >> 16) as u16);
        snes.set_player_2(Buttons::from_bits(two));
        if snes.run_frame(Buttons::from_bits(one)).is_err() {
            crate::app_log::error("XSnes3D+: the SNES core failed during a frame.");
            return self.frames;
        }
        self.frames += 1;
        self.sound.clear();
        snes.take_audio(&mut self.sound);
        self.audio.push(&self.sound);
        self.picture = snes.frame();
        self.input = snes.tile_capture().map(|capture| {
            let mut input = capture.engine_input();
            if let Some(frame) = &self.picture {
                let look = capture.look(frame);
                capture.apply_finished_colors(&mut input, &look);
                capture.add_backdrop(&mut input, frame, &look);
            }
            input
        });
        self.frames
    }

    /// [`SnesCore::run_frame`]: the SNES core has no run-ahead yet, so `ahead` is not used.
    pub fn run_frame_ahead(&mut self, word: PadWord, ahead: u8) -> i64 {
        let _ = ahead;
        self.run_frame(word)
    }

    /// Whether a frame in advance is being shown (run-ahead): never yet on the SNES.
    #[must_use]
    pub const fn holds_a_real_frame(&self) -> bool {
        false
    }

    /// Puts the real frame back after a run-ahead: nothing to do yet on the SNES.
    pub const fn restore_real_frame(&mut self) -> bool {
        false
    }

    /// The 3D layers of the last frame.
    #[must_use]
    pub const fn engine_input(&self) -> Option<&EngineInput> {
        self.input.as_ref()
    }

    /// The last frame's picture.
    #[must_use]
    pub const fn picture(&self) -> Option<&zax_snes::Frame> {
        self.picture.as_ref()
    }

    /// The sound's end for the audio stream.
    #[must_use]
    pub fn audio_reader(&self) -> AudioReader {
        self.audio.clone()
    }

    /// The whole console state.
    ///
    /// # Errors
    ///
    /// No game, or Mesen could not make it.
    pub fn save_state(&mut self) -> Result<Vec<u8>, String> {
        let snes = self.snes.as_mut().ok_or("no game is running")?;
        let mut state = snes.save_state().map_err(|error| error.to_string())?;
        // The frame count goes with it.
        state.extend_from_slice(&self.frames.to_le_bytes());
        Ok(state)
    }

    /// Puts back a state [`SnesCore::save_state`] made.
    ///
    /// # Errors
    ///
    /// No game, or the state is not one of this game.
    pub fn load_state(&mut self, state: &[u8]) -> Result<(), String> {
        let snes = self.snes.as_mut().ok_or("no game is running")?;
        let split = state.len().checked_sub(8).ok_or("the state is too short")?;
        let (mesen, count) = state.split_at(split);
        snes.load_state(mesen).map_err(|error| error.to_string())?;
        let mut bytes = [0u8; 8];
        bytes.copy_from_slice(count);
        self.frames = i64::from_le_bytes(bytes);
        self.audio.clear();
        Ok(())
    }

    /// The console's reset button.
    pub fn soft_reset(&mut self) {
        if let Some(snes) = self.snes.as_mut() {
            if snes.reset().is_err() {
                crate::app_log::error("XSnes3D+: the SNES core could not reset.");
            }
        }
    }

    /// Off and on again: the game is loaded anew.
    pub fn reset(&mut self) {
        if let Some(image) = self.image.take() {
            let name = "game.sfc";
            let _ = self.load_rom(&image, name);
        }
    }

    /// Frames a second this game runs at.
    #[must_use]
    pub const fn frame_rate(&self) -> f64 {
        NTSC_FRAME_RATE
    }
}

fn failure(error: String) -> RomLoadResult {
    RomLoadResult {
        success: false,
        error: Some(error),
    }
}

/// A file name Mesen can load: the name given, without folders, with a SNES extension when it
/// has none Mesen knows.
fn file_name(name: &str) -> String {
    let base = Path::new(name)
        .file_name()
        .map_or_else(|| "game".to_owned(), |n| n.to_string_lossy().into_owned());
    let lower = base.to_ascii_lowercase();
    if [".sfc", ".smc", ".zip", ".7z", ".swc", ".fig", ".bs"]
        .iter()
        .any(|ext| lower.ends_with(ext))
    {
        base
    } else {
        format!("{base}.sfc")
    }
}

/// The core's home folder until the host gives one ([`SnesCore::set_home`]).
fn default_home() -> PathBuf {
    std::env::temp_dir().join("xsnes3d")
}

#[cfg(test)]
mod tests {
    use super::{file_name, AudioReader};

    #[test]
    fn a_game_file_keeps_its_name_or_gets_a_snes_extension() {
        assert_eq!(file_name("Mario.sfc"), "Mario.sfc");
        assert_eq!(file_name("roms/Zelda.SMC"), "Zelda.SMC");
        assert_eq!(file_name("Game.zip"), "Game.zip");
        assert_eq!(file_name("odd"), "odd.sfc");
    }

    #[test]
    fn sound_is_read_whole_or_not_at_all() {
        let audio = AudioReader::default();
        audio.push(&[16_384, -16_384, 0, 0]);
        let mut out = [0.0f32; 6];
        assert_eq!(audio.read_samples(&mut out, true), 0);
        let mut out = [0.0f32; 4];
        assert_eq!(audio.read_samples(&mut out, true), 4);
        assert!((out[0] - 0.5).abs() < 1e-6 && (out[1] + 0.5).abs() < 1e-6);
        assert_eq!(audio.available_samples(), 0);
    }
}
