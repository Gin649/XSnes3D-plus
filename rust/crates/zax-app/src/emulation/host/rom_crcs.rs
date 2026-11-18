//! The CRC-32 of the ROM files (Rust-only, owner request), kept in a cache of the user data.

use std::path::Path;

use super::{EmulationHost, GameView};
use crate::emulation::game_catalog::GameCatalog;

impl<C: GameCatalog, V: GameView> EmulationHost<C, V> {
    /// The CRC-32 of the ROM in `path`, read from the file only when the cache has not seen it
    /// as it is; none when it cannot be read.
    #[must_use]
    pub fn rom_data_crc(&self, path: &Path) -> Option<u32> {
        self.rom_crcs.borrow_mut().crc_of(path)
    }

    /// The CRC-32 the cache keeps for the ROM in `path`, without reading the file: none when
    /// the cache has not seen it as it is.
    #[must_use]
    pub fn known_rom_crc(&self, path: &Path) -> Option<u32> {
        self.rom_crcs.borrow_mut().known(path)
    }

    /// Writes the cache of the ROM CRCs when it learned something.
    pub fn save_rom_crcs(&self) {
        let written = self.rom_crcs.borrow_mut().save();
        if let Err(error) = written {
            self.log(&format!("the ROM CRC cache could not be written: {error}"));
        }
    }
}
