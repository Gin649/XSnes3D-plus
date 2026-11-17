//! Boots tests/roms/hello.sfc (built by tools/make_hello.py) and checks its picture.
//!
//! Everything runs in one test, because only one emulator may exist at a time.

use std::path::PathBuf;

use zax_snes::{Buttons, Snes};

const BACKDROP: [u8; 3] = [0, 0, 165];
const YELLOW: [u8; 3] = [255, 255, 0];

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

    // The rectangle covers pixels 32 to 95 across and 32 to 63 down.
    assert_eq!(first.pixel(32, 32), Some(YELLOW));
    assert_eq!(first.pixel(95, 63), Some(YELLOW));
    assert_eq!(first.pixel(60, 48), Some(YELLOW));
    // Just outside it, and far away, is the backdrop.
    for (x, y) in [(31, 40), (96, 40), (40, 31), (40, 64), (0, 0), (255, 223)] {
        assert_eq!(first.pixel(x, y), Some(BACKDROP), "pixel {x},{y}");
    }

    let second = boot(60);
    assert_eq!(
        first.hash(),
        second.hash(),
        "two runs gave different pictures"
    );
}
