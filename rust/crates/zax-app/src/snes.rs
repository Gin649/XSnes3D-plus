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

/// Sound kept for the audio stream at most, in stereo frames (half a second at the highest
/// usual rate).
const RING_FRAMES: usize = 24_000;

/// The 2D picture's size, as the renderer takes it (the NES app's frame).
const PICTURE_WIDTH: usize = 256;
const PICTURE_HEIGHT: usize = 240;

/// The least sound the audio stream's ring is sized for, in frames.
pub const MIN_BUFFER_SAMPLES: usize = 10_000;

/// The pads of both players in one word: player 1 in bits 0 to 11, player 2 in bits 16 to
/// 27, each in Mesen's order (A, B, X, Y, L, R, Select, Start, Up, Down, Left, Right).
pub type PadWord = u32;

/// The TV region the settings name. The NES app's four (Dendy and the dual-region "Multi"
/// carts are the NES's); the SNES core reads its game's own region, so this only steers the
/// frame pacer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TvRegion {
    /// 60 frames a second.
    #[default]
    Ntsc,
    /// 50 frames a second.
    Pal,
    /// Either; played as NTSC.
    Multi,
    /// The NES clones' PAL timing; played as PAL.
    Dendy,
}

/// What a ROM load gave.
#[derive(Debug, Clone, Default)]
pub struct RomLoadResult {
    /// Whether the game runs.
    pub success: bool,
    /// Why not, when it does not.
    pub error: Option<String>,
}

/// Sound made by the core and not played yet: stereo frames, left first, -1 to 1, at the
/// rate the stream asked for (Mesen mixes at [`SAMPLE_RATE`]; [`SampleRing::push`] resamples).
struct SampleRing {
    samples: std::collections::VecDeque<f32>,
    /// The stream's rate.
    rate: f64,
    /// Where the next output frame falls between the last input frame and the next one.
    phase: f64,
    /// The last input frame.
    last: [f32; 2],
    /// The pacer's nudge: above 0, a little more sound is made per frame (the stream's buffer
    /// is running low); below 0, a little less.
    adjustment: f64,
}

impl Default for SampleRing {
    fn default() -> Self {
        Self {
            samples: std::collections::VecDeque::new(),
            rate: f64::from(SAMPLE_RATE),
            phase: 0.0,
            last: [0.0; 2],
            adjustment: 0.0,
        }
    }
}

impl SampleRing {
    /// Adds Mesen's stereo frames, resampled to the stream's rate (straight lines between
    /// frames: Mesen's own output is already filtered).
    fn push(&mut self, samples: &[i16]) {
        let step = f64::from(SAMPLE_RATE) / (self.rate * (1.0 + self.adjustment));
        for frame in samples.chunks_exact(2) {
            let current = [f32::from(frame[0]) / 32_768.0, f32::from(frame[1]) / 32_768.0];
            while self.phase < 1.0 {
                #[allow(clippy::cast_possible_truncation)] // Between 0 and 1.
                let t = self.phase as f32;
                for channel in 0..2 {
                    let value = self.last[channel] + (current[channel] - self.last[channel]) * t;
                    self.samples.push_back(value);
                }
                self.phase += step;
            }
            self.phase -= 1.0;
            self.last = current;
        }
        // The oldest sound goes when the stream falls behind.
        let over = self.samples.len().saturating_sub(RING_FRAMES * 2);
        self.samples.drain(..over);
    }
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
    /// Fills `destination` (stereo pairs when `stereo`, else the two channels mixed) when that
    /// much sound is waiting; returns how many values were written: all of them, or 0 when not
    /// enough sound is there yet.
    ///
    /// # Errors
    ///
    /// An odd-sized stereo buffer.
    pub fn read_samples(&self, destination: &mut [f32], stereo: bool) -> Result<usize, String> {
        if stereo && destination.len() % 2 != 0 {
            return Err("a stereo buffer holds pairs".to_owned());
        }
        let mut ring = lock(&self.ring);
        let frames = if stereo {
            destination.len() / 2
        } else {
            destination.len()
        };
        if frames == 0 || ring.samples.len() / 2 < frames {
            return Ok(0);
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
        Ok(if stereo { frames * 2 } else { frames })
    }

    /// Frames waiting (one value each once mixed to mono).
    #[must_use]
    pub fn available_samples(&self) -> usize {
        lock(&self.ring).samples.len() / 2
    }

    fn push(&self, samples: &[i16]) {
        lock(&self.ring).push(samples);
    }

    fn clear(&self) {
        lock(&self.ring).samples.clear();
    }

    fn set_adjustment(&self, adjustment: f64) {
        lock(&self.ring).adjustment = adjustment;
    }

    fn set_rate(&self, rate: i32) {
        let mut ring = lock(&self.ring);
        ring.rate = f64::from(rate.max(8_000));
        ring.samples.clear();
        ring.phase = 0.0;
    }

    fn rate(&self) -> i32 {
        #[allow(clippy::cast_possible_truncation)] // A sample rate.
        let rate = lock(&self.ring).rate as i32;
        rate
    }
}

/// The SNES core of one game.
pub struct SnesCore {
    snes: Option<Snes>,
    home: PathBuf,
    image: Option<Vec<u8>>,
    /// The ROM's file name in the home folder's `Games` (Mesen keeps the battery save under it).
    name: String,
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
            name: String::new(),
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
                self.name = file_name(name);
                self.frames = 0;
                RomLoadResult {
                    success: true,
                    error: None,
                }
            }
            Err(error) => failure(error.to_string()),
        }
    }

    /// Whether `image`, from the file `name`, can be a SNES game: an archive Mesen opens, or a
    /// ROM of at least 32 KiB (with or without the 512-byte copier header).
    #[must_use]
    pub fn looks_like_a_game(image: &[u8], name: &str) -> bool {
        let lower = name.to_ascii_lowercase();
        if lower.ends_with(".zip") || lower.ends_with(".7z") {
            return !image.is_empty();
        }
        image.len() >= 0x8000 && image.len() % 0x400 != 0x200 || image.len() >= 0x8200
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
        let snes = self.snes.as_ref().ok_or("no game is running")?;
        let state = snes.save_state().map_err(|error| error.to_string())?;
        buffer.clear();
        buffer.extend_from_slice(&state);
        // A state on disk carries its game (the ROM and its file name), so it can be loaded
        // when no game runs yet: the resume point and the slots of the game list. The file
        // name matters, as Mesen keeps the battery save under it. Then, at the end, the
        // lengths and the frame count, so they are found from the end:
        // Mesen's state | ROM | name | name length (u32) | ROM length (u64) | frame count (i64).
        let (rom, name) = match (&self.image, include_rom_image) {
            (Some(image), true) => (image.as_slice(), self.name.as_bytes()),
            _ => (&[][..], &[][..]),
        };
        buffer.extend_from_slice(rom);
        buffer.extend_from_slice(name);
        buffer.extend_from_slice(&u32::try_from(name.len()).unwrap_or(0).to_le_bytes());
        buffer.extend_from_slice(&(rom.len() as u64).to_le_bytes());
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
        const SHORT: &str = "the state is too short";
        let state = data.get(offset..).ok_or(SHORT)?;
        // The layout of [`SnesCore::save_state_to`], read from the end.
        let tail = |at: usize, size: usize| -> Result<&[u8], String> {
            let end = state.len().checked_sub(at).ok_or(SHORT)?;
            let start = end.checked_sub(size).ok_or(SHORT)?;
            Ok(&state[start..end])
        };
        let word = |bytes: &[u8]| {
            let mut eight = [0u8; 8];
            eight[..bytes.len()].copy_from_slice(bytes);
            u64::from_le_bytes(eight)
        };
        let frames = word(tail(0, 8)?);
        let rom_length = usize::try_from(word(tail(8, 8)?)).map_err(|_| SHORT)?;
        let name_length = usize::try_from(word(tail(16, 4)?)).map_err(|_| SHORT)?;
        let name = tail(20, name_length)?;
        let rom = tail(20 + name_length, rom_length)?;
        let mesen_end = state.len() - 20 - name_length - rom_length;
        let mesen = &state[..mesen_end];
        // A state that carries its game loads it first when it is not the one running.
        if !rom.is_empty() && self.image.as_deref() != Some(rom) {
            let name = String::from_utf8_lossy(name).into_owned();
            let loaded = self.load_rom(rom, &name);
            if !loaded.success {
                return Err(loaded.error.unwrap_or_else(|| "the game did not load".to_owned()));
            }
        }
        let snes = self.snes.as_mut().ok_or("no game is running")?;
        snes.load_state(mesen).map_err(|error| error.to_string())?;
        #[allow(clippy::cast_possible_wrap)] // Written from an i64.
        {
            self.frames = frames as i64;
        }
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
            let name = self.name.clone();
            let _ = self.load_rom(&image, &name);
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

    /// The sample rate of the sound the stream reads.
    #[must_use]
    pub fn sample_rate(&self) -> i32 {
        self.audio.rate()
    }

    /// The sample rate the stream reads at: Mesen mixes at [`SAMPLE_RATE`], and the sound is
    /// resampled to this one.
    ///
    /// # Errors
    ///
    /// A rate under 8 kHz or over 384 kHz.
    pub fn set_sample_rate(&mut self, rate: i32) -> Result<(), String> {
        if !(8_000..=384_000).contains(&rate) {
            return Err(format!("sample rate {rate} is out of range"));
        }
        self.audio.set_rate(rate);
        Ok(())
    }

    /// The nudge of the sound's speed the pacer asked for last.
    #[must_use]
    pub const fn audio_ratio_adjustment(&self) -> f64 {
        self.ratio_adjustment
    }

    /// Nudges the sound's speed to keep the stream's buffer steady: the resampling to the
    /// stream's rate makes that much more (or less) sound from each frame.
    ///
    /// # Errors
    ///
    /// A nudge past [`MAX_RATIO_ADJUSTMENT`].
    pub fn set_audio_ratio_adjustment(&mut self, value: f64) -> Result<(), String> {
        if value.abs() > MAX_RATIO_ADJUSTMENT {
            return Err(format!("ratio adjustment {value} is too large"));
        }
        self.ratio_adjustment = value;
        self.audio.set_adjustment(value);
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
    pub fn set_region<T>(&mut self, region: T) {
        let _ = region;
    }

    /// The core's name in a savestate's header.
    pub const CORE_NAME: &'static str = "XSnes3D+ Mesen 2.1.1";

    /// The version of the core state in a savestate: Mesen's own state format inside.
    pub const STATE_VERSION: i32 = 1;

    /// The largest core state a savestate may hold: Mesen's SNES state with its chips (Super FX,
    /// SA-1) and their memory is under 2 MiB.
    #[must_use]
    pub const fn max_state_size(&self) -> usize {
        // Mesen's state, and the ROM when the state carries it (up to 8 MiB on the SNES).
        16 * 1024 * 1024
    }

    /// The most frames of run-ahead the core offers: none yet on the SNES.
    pub const MAX_RUN_AHEAD: u8 = 0;

    /// A byte of the game's memory for the scripts' `ReadMem`: the first 64 KiB of work RAM,
    /// bank $7E (the bank games keep their variables in; $0000 to $1FFF is also what banks $00
    /// to $3F show at those addresses). 0 without a game.
    #[must_use]
    pub fn read_memory(&self, address: u16) -> u8 {
        self.snes
            .as_ref()
            .and_then(|snes| snes.work_ram().get(usize::from(address)).copied())
            .unwrap_or(0)
    }

    /// A write of the scripts' `WriteMem`, to the same bytes [`SnesCore::read_memory`] reads.
    pub fn write_memory(&mut self, address: u16, value: u8) {
        if let Some(byte) = self
            .snes
            .as_mut()
            .and_then(|snes| snes.work_ram_mut().get_mut(usize::from(address)))
        {
            *byte = value;
        }
    }

    /// A byte as RetroAchievements addresses a SNES: work RAM from $000000 to $01FFFF, then
    /// the save RAM from $020000. `None` past the end.
    #[must_use]
    pub fn achievement_memory(&self, address: u32) -> Option<u8> {
        const WORK_RAM: usize = 0x2_0000;
        let snes = self.snes.as_ref()?;
        let address = usize::try_from(address).ok()?;
        if address < WORK_RAM {
            snes.work_ram().get(address).copied()
        } else {
            snes.save_ram().get(address - WORK_RAM).copied()
        }
    }

    /// Copies the 2D picture into `out`: 256 by 240 pixels, red, green, blue and alpha. The
    /// SNES picture's 224 (or 239) lines are the top rows and the rest is left out (alpha 0);
    /// a hi-res picture is halved each way. Returns the rows, 240.
    pub fn wide_picture_into(&self, out: &mut Vec<u8>) -> usize {
        out.clear();
        out.resize(PICTURE_WIDTH * PICTURE_HEIGHT * 4, 0);
        let Some(frame) = &self.picture else {
            return PICTURE_HEIGHT;
        };
        let width = frame.width as usize;
        let height = frame.height as usize;
        let step_x = if width >= 2 * PICTURE_WIDTH { 2 } else { 1 };
        let step_y = if height > PICTURE_HEIGHT + 16 { 2 } else { 1 };
        let rows = (height / step_y).min(PICTURE_HEIGHT);
        let columns = (width / step_x).min(PICTURE_WIDTH);
        for y in 0..rows {
            let source = y * step_y * width;
            let target = y * PICTURE_WIDTH;
            for x in 0..columns {
                let from = (source + x * step_x) * 3;
                let to = (target + x) * 4;
                if let Some(rgb) = frame.rgb.get(from..from + 3) {
                    out[to..to + 3].copy_from_slice(rgb);
                    out[to + 3] = 255;
                }
            }
        }
        PICTURE_HEIGHT
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
    fn sound_is_resampled_to_the_stream_rate() {
        let audio = AudioReader::default();
        audio.set_rate(24_000);
        audio.push(&[0; 2 * 4800]);
        assert_eq!(audio.available_samples(), 2400);
    }

    #[test]
    fn sound_is_read_whole_or_not_at_all() {
        let audio = AudioReader::default();
        // At Mesen's own rate a frame in is a frame out, one frame late (the line starts at
        // the frame before, silence).
        audio.push(&[16_384, -16_384, 16_384, -16_384]);
        let mut out = [0.0f32; 6];
        assert_eq!(audio.read_samples(&mut out, true), Ok(0));
        let mut out = [0.0f32; 4];
        assert_eq!(audio.read_samples(&mut out, true), Ok(4));
        assert!(out[0].abs() < 1e-6 && (out[2] - 0.5).abs() < 1e-6 && (out[3] + 0.5).abs() < 1e-6);
        assert_eq!(audio.available_samples(), 0);
        assert!(audio.read_samples(&mut [0.0; 3], true).is_err());
    }
}
