//! The SNES core as the app sees it: `zax_snes::Snes` with what the app keeps beside it (the
//! frame's picture and 3D layers, the sound waiting for the audio stream, the ROM it loaded).
//!
//! The NES app talked to its own core here; the SNES runs Mesen 2 through `zax-snes`. A game
//! is loaded from the bytes the app read, through a copy in the core's home folder, since
//! Mesen loads files.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use zax_engine3d::palette::Rgb24;
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
    sky: Rgb24,
    speed: f64,
    ratio_adjustment: f64,
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
            sky: Rgb24::default(),
            speed: 1.0,
            ratio_adjustment: 0.0,
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
                if let Some([r, g, b]) = capture.sky_color(frame) {
                    self.sky = Rgb24::new(r, g, b);
                }
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

    /// The whole console state, with the frame count. `include_rom_image` is part of the NES
    /// app's interface: a SNES state never holds the game, which must be loaded to put it
    /// back.
    ///
    /// # Errors
    ///
    /// No game, or Mesen could not make it.
    pub fn save_state(&self, include_rom_image: bool) -> Result<Vec<u8>, String> {
        let mut state = Vec::new();
        self.save_state_to(include_rom_image, &mut state)?;
        Ok(state)
    }

    /// [`SnesCore::save_state`] into `buffer`, whose allocation is kept.
    ///
    /// # Errors
    ///
    /// No game, or Mesen could not make it; `buffer` is then untouched.
    pub fn save_state_to(&self, include_rom_image: bool, buffer: &mut Vec<u8>) -> Result<(), String> {
        let _ = include_rom_image;
        let snes = self.snes.as_ref().ok_or("no game is running")?;
        let state = snes.save_state().map_err(|error| error.to_string())?;
        buffer.clear();
        buffer.extend_from_slice(&state);
        // The frame count goes with it.
        buffer.extend_from_slice(&self.frames.to_le_bytes());
        Ok(())
    }

    /// [`SnesCore::save_state`] where the game stands; the NES app ran on to the game's idle
    /// loop first, which the SNES does not know, so the answer's `bool` is always `false`.
    ///
    /// # Errors
    ///
    /// No game, or Mesen could not make it.
    pub fn save_state_at_idle(&mut self, include_rom_image: bool) -> Result<(Vec<u8>, bool), String> {
        Ok((self.save_state(include_rom_image)?, false))
    }

    /// Puts back the state [`SnesCore::save_state`] made that starts at `offset` in `data`.
    ///
    /// # Errors
    ///
    /// No game, or the state is not one of this game.
    pub fn try_load_state(&mut self, data: &[u8], offset: usize) -> Result<(), String> {
        let state = data.get(offset..).ok_or("the state is too short")?;
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

    /// Whole frames a second this game runs at, as the frame pacer counts them.
    #[must_use]
    pub const fn nominal_frame_rate(&self) -> i32 {
        60
    }

    /// The sky colour of the last picture (the scene's background colour).
    #[must_use]
    pub const fn background_color(&self) -> Rgb24 {
        self.sky
    }

    /// The sound waiting for the stream, read by the frame pacer ([`AudioReader`]).
    #[must_use]
    pub const fn apu(&self) -> &AudioReader {
        &self.audio
    }

    /// Drops the sound not played yet (a savestate put back, a new game).
    pub fn drop_pending_samples(&self) {
        self.audio.clear();
    }

    /// The game speed factor (1 is the console's). Mesen runs one frame per call whatever
    /// the factor; the frame pacer calls it faster or slower.
    ///
    /// # Errors
    ///
    /// A factor of 0 or less.
    pub fn set_speed(&mut self, factor: f64) -> Result<(), String> {
        if factor <= 0.0 {
            return Err(format!("speed factor {factor} is not positive"));
        }
        self.speed = factor;
        Ok(())
    }

    /// The game speed factor.
    #[must_use]
    pub const fn speed(&self) -> f64 {
        self.speed
    }

    /// The sample rate of the sound: Mesen's, whatever the stream asks.
    #[must_use]
    pub const fn sample_rate(&self) -> i32 {
        SAMPLE_RATE
    }

    /// Asks for a sample rate. Mesen mixes at [`SAMPLE_RATE`] and the audio stream resamples,
    /// so the request is accepted and changes nothing.
    ///
    /// # Errors
    ///
    /// Never; the signature is the NES app's.
    pub const fn set_sample_rate(&mut self, rate: i32) -> Result<(), String> {
        let _ = rate;
        Ok(())
    }

    /// The nudge of the sound's speed the pacer asked for last.
    #[must_use]
    pub const fn audio_ratio_adjustment(&self) -> f64 {
        self.ratio_adjustment
    }

    /// Nudges the sound's speed to keep the stream's buffer steady. Kept for the pacer; Mesen's
    /// own resampler does not take it yet.
    ///
    /// # Errors
    ///
    /// A nudge past [`MAX_RATIO_ADJUSTMENT`].
    pub fn set_audio_ratio_adjustment(&mut self, value: f64) -> Result<(), String> {
        if value.abs() > MAX_RATIO_ADJUSTMENT {
            return Err(format!("ratio adjustment {value} is too large"));
        }
        self.ratio_adjustment = value;
        Ok(())
    }

    /// The battery RAM the app keeps in its own file: Mesen keeps a SNES game's battery RAM
    /// itself (in `Saves` of the home folder), so there is none to hand over.
    #[must_use]
    pub const fn export_battery_ram(&self) -> &[u8] {
        &[]
    }

    /// Battery RAM from the app's own file: Mesen loads its own (see
    /// [`SnesCore::export_battery_ram`]).
    pub const fn import_battery_ram(&mut self, ram: &[u8]) {
        let _ = ram;
    }

    /// The TV region: Mesen reads it from the game, so the setting changes nothing.
    pub const fn set_region<T>(&mut self, region: T) {
        let _ = region;
    }

    /// The most frames of run-ahead the core offers: none yet on the SNES.
    pub const MAX_RUN_AHEAD: u8 = 0;

    /// A byte of the game's memory for the scripts' `ReadMem`. SNES profiles do not exist
    /// yet, so this reads 0 until the scripts get a SNES memory map.
    #[must_use]
    pub const fn read_memory(&self, address: u16) -> u8 {
        let _ = address;
        0
    }

    /// A write of the scripts' `WriteMem`: not done on the SNES yet (see
    /// [`SnesCore::read_memory`]).
    pub const fn write_memory(&mut self, address: u16, value: u8) {
        let _ = (address, value);
    }

    // The NES app's 2D picture path. The renderer's 2D picture is NES colour indices, which a
    // SNES picture is not; until it takes RGB pictures, the 2D picture is blank.

    /// Copies the 2D picture into `out` (256 by 240 colour indices); returns its rows.
    pub fn wide_picture_into(&self, out: &mut Vec<u8>) -> usize {
        out.clear();
        out.resize(256 * 240, 0);
        240
    }

    /// The smooth-scrolling shift of the 2D picture: none on the SNES.
    #[must_use]
    pub const fn picture_shift(&self) -> Option<(f32, i32, i32)> {
        None
    }

    /// The camera motion of the 2D picture: none on the SNES.
    #[must_use]
    pub const fn picture_motion(&self) -> Option<f32> {
        None
    }

    /// The camera of the 2D picture: none on the SNES.
    #[must_use]
    pub const fn picture_camera(&self) -> Option<f64> {
        None
    }

    /// Moves the sprites that follow the camera out of the 2D picture: none on the SNES.
    pub fn split_steady_sprites(&self, picture: &mut [u8], overlay: &mut Vec<u8>) {
        let _ = picture;
        overlay.clear();
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
