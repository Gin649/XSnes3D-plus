//! The SNES core of `XSnes3D+`: Mesen 2's emulator, driven one frame at a time.
//!
//! [`Snes`] loads a ROM, takes the port 1 buttons, runs frames and hands back the finished
//! picture. Mesen itself is C++ and is built by `build.rs`; `cpp/shim.cpp` is the small C
//! interface this file calls.

use std::ffi::{c_char, c_int, CString};
use std::fmt;
use std::path::Path;
use std::sync::Mutex;

pub mod engine_input;
pub mod press;
pub mod tap;

pub use engine_input::{EngineInput, LayerInput, BACKDROP, BG3_FRONT};
pub use press::{Buttons, PressScript};
pub use tap::{Look, Rebuild, SnesTileRecord, TileCapture, TileLayer};

#[repr(C)]
struct RawSnes {
    _private: [u8; 0],
}

extern "C" {
    fn xs_create(home_folder: *const c_char) -> *mut RawSnes;
    fn xs_load_rom(snes: *mut RawSnes, path: *const c_char) -> c_int;
    fn xs_set_buttons(snes: *mut RawSnes, buttons: u16);
    fn xs_set_port_buttons(snes: *mut RawSnes, port: u32, buttons: u16);
    fn xs_take_audio(
        snes: *mut RawSnes,
        out: *mut i16,
        max_frames: u32,
        sample_rate: *mut u32,
    ) -> u32;
    fn xs_reset(snes: *mut RawSnes) -> c_int;
    fn xs_save_state(snes: *mut RawSnes, size: *mut u32) -> *const u8;
    fn xs_load_state(snes: *mut RawSnes, data: *const u8, size: u32) -> c_int;
    fn xs_run_frame(snes: *mut RawSnes) -> c_int;
    fn xs_frame(
        snes: *mut RawSnes,
        width: *mut u32,
        height: *mut u32,
        overscan: *mut c_int,
    ) -> *const u16;
    fn xs_set_tap(snes: *mut RawSnes, on: c_int) -> c_int;
    fn xs_tap_view(snes: *mut RawSnes, view: *mut tap::RawView);
    fn xs_destroy(snes: *mut RawSnes);
}

/// Mesen keeps some state in globals (its home folder, its message log), so only one
/// emulator may exist at a time. This lock is held for each emulator's whole life.
static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

/// What went wrong.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SnesError {
    /// The emulator could not be set up.
    Create,
    /// The path holds a character the C side cannot take (a zero byte).
    BadPath,
    /// Mesen could not load the file, or it is not a SNES game.
    Load(String),
    /// Mesen reported an error while running a frame.
    Frame,
    /// Another emulator is still alive in this process.
    Busy,
    /// The tile tap could not be switched on or off.
    Tap,
    /// A savestate could not be made or put back.
    State,
}

impl fmt::Display for SnesError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Create => write!(f, "the SNES emulator could not be set up"),
            Self::BadPath => write!(f, "the path holds a zero byte"),
            Self::Load(path) => write!(f, "{path} could not be loaded as a SNES game"),
            Self::Frame => write!(f, "the SNES emulator failed while running a frame"),
            Self::Busy => write!(f, "another SNES emulator is still running in this process"),
            Self::Tap => write!(f, "the tile tap could not be switched"),
            Self::State => write!(f, "the savestate could not be made or put back"),
        }
    }
}

impl std::error::Error for SnesError {}

/// One finished picture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    /// 256, or 512 in hi-res.
    pub width: u32,
    /// 224 or 239 lines (448 or 478 in hi-res), the picture the TV shows.
    pub height: u32,
    /// Red, green, blue, 8 bits each, row by row.
    pub rgb: Vec<u8>,
}

impl Frame {
    /// The colour at `x`, `y`, or `None` outside the picture.
    #[must_use]
    pub fn pixel(&self, x: u32, y: u32) -> Option<[u8; 3]> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let at = ((y * self.width + x) * 3) as usize;
        Some([self.rgb[at], self.rgb[at + 1], self.rgb[at + 2]])
    }

    /// A 64-bit FNV-1a hash of the picture, to compare runs.
    #[must_use]
    pub fn hash(&self) -> u64 {
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for byte in self
            .width
            .to_le_bytes()
            .iter()
            .chain(self.height.to_le_bytes().iter())
            .chain(self.rgb.iter())
        {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x0100_0000_01b3);
        }
        hash
    }
}

/// A running SNES.
pub struct Snes {
    raw: *mut RawSnes,
    frames: u32,
    tap_on: bool,
    _guard: std::sync::MutexGuard<'static, ()>,
}

impl Snes {
    /// Starts an emulator and loads `rom`. Mesen keeps its save files under `home_folder`.
    ///
    /// # Errors
    ///
    /// [`SnesError::Busy`] while another [`Snes`] is alive, [`SnesError::Create`] or
    /// [`SnesError::Load`] when Mesen fails, [`SnesError::BadPath`] for a path with a zero byte.
    pub fn load(rom: &Path, home_folder: &Path) -> Result<Self, SnesError> {
        let guard = ONE_AT_A_TIME.try_lock().map_err(|_| SnesError::Busy)?;
        let home = c_path(home_folder)?;
        let rom_path = c_path(rom)?;
        // SAFETY: `home` is a valid C string for the call; the shim copies it.
        let raw = unsafe { xs_create(home.as_ptr()) };
        if raw.is_null() {
            return Err(SnesError::Create);
        }
        let snes = Self {
            raw,
            frames: 0,
            tap_on: false,
            _guard: guard,
        };
        // SAFETY: `raw` came from `xs_create` and is not freed until `drop`.
        if unsafe { xs_load_rom(snes.raw, rom_path.as_ptr()) } != 1 {
            return Err(SnesError::Load(rom.display().to_string()));
        }
        Ok(snes)
    }

    /// Frames run since the ROM was loaded.
    #[must_use]
    pub fn frames(&self) -> u32 {
        self.frames
    }

    /// Runs one frame with `buttons` held on the port 1 pad.
    ///
    /// # Errors
    ///
    /// [`SnesError::Frame`] when Mesen reports an error.
    pub fn run_frame(&mut self, buttons: Buttons) -> Result<(), SnesError> {
        // SAFETY: `raw` is alive for as long as `self`.
        unsafe { xs_set_buttons(self.raw, buttons.bits()) };
        // SAFETY: as above.
        if unsafe { xs_run_frame(self.raw) } != 1 {
            return Err(SnesError::Frame);
        }
        self.frames += 1;
        Ok(())
    }

    /// Sets the buttons of the second player's pad (port 2), used from the next frame.
    pub fn set_player_2(&mut self, buttons: Buttons) {
        // SAFETY: `raw` is alive for as long as `self`.
        unsafe { xs_set_port_buttons(self.raw, 1, buttons.bits()) };
    }

    /// Moves the sound made since the last call to the end of `out`: 16-bit stereo samples,
    /// left then right. Returns their sample rate.
    pub fn take_audio(&mut self, out: &mut Vec<i16>) -> u32 {
        let mut chunk = [0i16; 2 * 1024];
        let mut rate = 0;
        loop {
            // SAFETY: `raw` is alive; `chunk` holds 1024 stereo frames.
            let frames =
                unsafe { xs_take_audio(self.raw, chunk.as_mut_ptr(), 1024, &raw mut rate) };
            out.extend_from_slice(&chunk[..frames as usize * 2]);
            if frames < 1024 {
                return rate;
            }
        }
    }

    /// Presses the console's reset button.
    ///
    /// # Errors
    ///
    /// [`SnesError::Frame`] when Mesen reports an error.
    pub fn reset(&mut self) -> Result<(), SnesError> {
        // SAFETY: `raw` is alive for as long as `self`.
        if unsafe { xs_reset(self.raw) } != 1 {
            return Err(SnesError::Frame);
        }
        Ok(())
    }

    /// The whole console state, to put back later with [`Snes::load_state`].
    ///
    /// # Errors
    ///
    /// [`SnesError::State`] when Mesen cannot make it.
    pub fn save_state(&mut self) -> Result<Vec<u8>, SnesError> {
        let mut size = 0;
        // SAFETY: `raw` is alive; the shim returns `size` bytes valid until the next save.
        let data = unsafe { xs_save_state(self.raw, &raw mut size) };
        if data.is_null() {
            return Err(SnesError::State);
        }
        // SAFETY: see above; the bytes are copied right away.
        Ok(unsafe { std::slice::from_raw_parts(data, size as usize) }.to_vec())
    }

    /// Puts back a state [`Snes::save_state`] made.
    ///
    /// # Errors
    ///
    /// [`SnesError::State`] when Mesen refuses it (another game, a damaged state).
    pub fn load_state(&mut self, state: &[u8]) -> Result<(), SnesError> {
        let size = u32::try_from(state.len()).map_err(|_| SnesError::State)?;
        // SAFETY: `raw` is alive; `state` is valid for `size` bytes during the call.
        if unsafe { xs_load_state(self.raw, state.as_ptr(), size) } != 1 {
            return Err(SnesError::State);
        }
        Ok(())
    }

    /// Starts or stops the tile tap: while it is on, each frame records the tiles it drew
    /// (see [`Snes::tile_capture`]). It costs some speed, so it is off by default.
    ///
    /// # Errors
    ///
    /// [`SnesError::Tap`] when Mesen has no SNES picture unit to attach to.
    pub fn set_tile_tap(&mut self, on: bool) -> Result<(), SnesError> {
        // SAFETY: `raw` is alive for as long as `self`.
        if unsafe { xs_set_tap(self.raw, c_int::from(on)) } != 1 {
            return Err(SnesError::Tap);
        }
        self.tap_on = on;
        Ok(())
    }

    /// What the tile tap recorded during the last frame, or `None` when it is off or no frame
    /// has run since it was switched on.
    #[must_use]
    pub fn tile_capture(&self) -> Option<TileCapture> {
        if !self.tap_on || self.frames == 0 {
            return None;
        }
        let mut view = tap::RawView {
            chunks: std::ptr::null(),
            chunk_count: 0,
            lines: std::ptr::null(),
            line_count: 0,
            sprites: std::ptr::null(),
            sprite_count: 0,
            cgram: std::ptr::null(),
            cgram_snapshots: 0,
            mode7: std::ptr::null(),
            mode7_count: 0,
        };
        // SAFETY: `raw` is alive; the shim fills `view` with pointers into its own buffers,
        // valid until the next frame, and `from_view` copies them right away.
        unsafe {
            xs_tap_view(self.raw, &raw mut view);
            Some(TileCapture::from_view(&view))
        }
    }

    /// The picture of the last frame run, or `None` before the first frame.
    #[must_use]
    pub fn frame(&self) -> Option<Frame> {
        if self.frames == 0 {
            return None;
        }
        let mut width = 0;
        let mut height = 0;
        let mut overscan = 0;
        // SAFETY: `raw` is alive; the shim fills the three numbers and returns a buffer of
        // `width * height` colours that stays valid until the next frame.
        let buffer =
            unsafe { xs_frame(self.raw, &raw mut width, &raw mut height, &raw mut overscan) };
        if buffer.is_null() || width == 0 || height == 0 {
            return None;
        }
        // SAFETY: see above; nothing runs a frame while this slice is alive.
        let colours = unsafe { std::slice::from_raw_parts(buffer, (width * height) as usize) };
        Some(crop_to_rgb(colours, width, height, overscan != 0))
    }
}

impl Drop for Snes {
    fn drop(&mut self) {
        // SAFETY: `raw` came from `xs_create` and is freed only here.
        unsafe { xs_destroy(self.raw) };
    }
}

fn c_path(path: &Path) -> Result<CString, SnesError> {
    CString::new(path.to_string_lossy().as_bytes()).map_err(|_| SnesError::BadPath)
}

/// Mesen's buffer is 239 lines (478 in hi-res). With overscan the game fills all of them.
/// Without it, Mesen puts the 224 lines the TV shows at rows 7 to 230, the rest stays black.
fn crop_to_rgb(colours: &[u16], width: u32, height: u32, overscan: bool) -> Frame {
    let scale = if height >= 478 { 2 } else { 1 };
    let (first, lines) = if overscan {
        (0, 239 * scale)
    } else {
        (7 * scale, 224 * scale)
    };
    let first = first.min(height);
    let lines = lines.min(height - first);
    let mut rgb = Vec::with_capacity((width * lines * 3) as usize);
    for y in first..first + lines {
        let row = (y * width) as usize;
        for &colour in &colours[row..row + width as usize] {
            rgb.extend_from_slice(&bgr555_to_rgb(colour));
        }
    }
    Frame {
        width,
        height: lines,
        rgb,
    }
}

/// A 15-bit SNES colour to 8-bit red, green and blue, the top bits repeated in the low ones.
#[must_use]
pub fn bgr555_to_rgb(colour: u16) -> [u8; 3] {
    let widen = |five: u16| -> u8 {
        let five = (five & 0x1f) as u8;
        (five << 3) | (five >> 2)
    };
    [widen(colour), widen(colour >> 5), widen(colour >> 10)]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colours_widen_to_full_range() {
        assert_eq!(bgr555_to_rgb(0), [0, 0, 0]);
        assert_eq!(bgr555_to_rgb(0x7fff), [255, 255, 255]);
        assert_eq!(bgr555_to_rgb(0x001f), [255, 0, 0]);
        assert_eq!(bgr555_to_rgb(20 << 10), [0, 0, 165]);
    }

    #[test]
    fn crop_keeps_the_224_shown_lines() {
        let mut colours = vec![0u16; 256 * 239];
        colours[7 * 256] = 0x7fff;
        let frame = crop_to_rgb(&colours, 256, 239, false);
        assert_eq!(frame.height, 224);
        assert_eq!(frame.pixel(0, 0), Some([255, 255, 255]));
    }
}
