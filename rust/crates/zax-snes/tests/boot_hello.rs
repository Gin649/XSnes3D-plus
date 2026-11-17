//! Boots `tests/roms/hello.sfc` (built by `tools/make_hello.py`) and checks its picture.
//!
//! Everything runs in one test, because only one emulator may exist at a time.

// The helpers below are test code too, so a failed step may stop the test.
#![allow(clippy::unwrap_used)]

use std::path::PathBuf;

use zax_snes::{Buttons, Snes};

const BACKDROP: [u8; 3] = [0, 0, 165];
const YELLOW: [u8; 3] = [255, 255, 0];
const GREEN: [u8; 3] = [0, 255, 0];
const RED: [u8; 3] = [255, 0, 0];

fn rom() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../tests/roms/hello.sfc")
}

fn home() -> PathBuf {
    let home = std::env::temp_dir().join(format!("xsnes3d-test-{}", std::process::id()));
    std::fs::create_dir_all(&home).unwrap();
    home
}

fn boot(frames: u32) -> zax_snes::Frame {
    let mut snes = Snes::load(&rom(), &home()).unwrap();
    assert!(snes.frame().is_none(), "a picture before the first frame");
    for _ in 0..frames {
        snes.run_frame(Buttons::NONE).unwrap();
    }
    assert_eq!(snes.frames(), frames);
    // While this one is alive, a second emulator is refused instead of sharing Mesen's
    // global state.
    assert!(matches!(
        Snes::load(&rom(), &home()),
        Err(zax_snes::SnesError::Busy)
    ));
    snes.frame().unwrap()
}

#[test]
fn hello_rom_draws_its_rectangle_the_same_way_every_time() {
    let first = boot(60);
    assert_eq!((first.width, first.height), (256, 224));

    // Background 1: the rectangle, pixels 32 to 95 across and 32 to 63 down, in front of the band.
    for (x, y) in [(32, 32), (95, 63), (60, 48)] {
        assert_eq!(first.pixel(x, y), Some(YELLOW), "pixel {x},{y}");
    }
    // Background 3: the band, all across, 48 to 79 down.
    for (x, y) in [(10, 52), (40, 64), (40, 79), (119, 52), (128, 52)] {
        assert_eq!(first.pixel(x, y), Some(GREEN), "pixel {x},{y}");
    }
    // The sprite: 120 to 127 across, 52 to 59 down, in front of the band.
    for (x, y) in [(120, 52), (127, 59)] {
        assert_eq!(first.pixel(x, y), Some(RED), "pixel {x},{y}");
    }
    // Everywhere else is the backdrop.
    for (x, y) in [(31, 40), (96, 40), (40, 31), (40, 80), (0, 0), (255, 223)] {
        assert_eq!(first.pixel(x, y), Some(BACKDROP), "pixel {x},{y}");
    }

    let second = boot(60);
    assert_eq!(
        first.hash(),
        second.hash(),
        "two runs gave different pictures"
    );
}
