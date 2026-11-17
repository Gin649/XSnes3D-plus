//! Records the tiles of `tests/roms/hello.sfc` with the tile tap, checks the records, and
//! checks that the picture rebuilt from them matches the PPU's own, pixel for pixel.

// The helpers below are test code too, so a failed step may stop the test.
#![allow(clippy::unwrap_used)]

use std::path::PathBuf;

use zax_snes::{Buttons, Snes, TileLayer};

#[test]
fn hello_tiles_are_recorded_and_rebuild_the_picture() {
    let rom = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../tests/roms/hello.sfc");
    let home = std::env::temp_dir().join(format!("xsnes3d-tap-{}", std::process::id()));
    std::fs::create_dir_all(&home).unwrap();
    let mut snes = Snes::load(&rom, &home).unwrap();
    assert!(snes.tile_capture().is_none(), "a capture with the tap off");
    snes.set_tile_tap(true).unwrap();
    for _ in 0..60 {
        snes.run_frame(Buttons::NONE).unwrap();
    }
    let capture = snes.tile_capture().unwrap();
    assert_eq!(capture.line_count(), 224);

    // The rebuilt picture matches, every pixel checked.
    let rebuild = capture.rebuild();
    assert_eq!((rebuild.width, rebuild.height), (256, 224));
    assert_eq!(rebuild.skipped, 0);
    assert_eq!(rebuild.checked, 256 * 224);
    assert_eq!(rebuild.mismatched, 0);
    assert_eq!(rebuild.rebuilt, rebuild.reference);
    // And the reference is the same picture the emulator shows (no colour maths here).
    assert_eq!(rebuild.reference, snes.frame().unwrap().rgb);

    let records = capture.records();
    // Background 1, tile 1 of the rectangle's top-left corner, 4 bits per pixel.
    assert!(records.iter().any(|r| r.layer == TileLayer::Background(0)
        && (r.x, r.y) == (32, 32)
        && r.tile == 1
        && r.bpp == 4));
    // Background 3, the band, palette 1, 2 bits per pixel.
    assert!(records.iter().any(|r| r.layer == TileLayer::Background(2)
        && (r.x, r.y) == (0, 48)
        && r.palette == 1
        && r.bpp == 2));
    // The sprite: 8 rows at 120 across, from line 52, priority 3.
    let sprite_rows: Vec<_> = records
        .iter()
        .filter(|r| r.layer == TileLayer::Sprite && r.x == 120)
        .collect();
    assert_eq!(sprite_rows.len(), 8);
    assert!(sprite_rows
        .iter()
        .all(|r| r.priority == 3 && (52..60).contains(&r.y)));
    // Background 1 has 32 tile columns on each of the 224 lines (with no fine scroll, the
    // 33rd column starts past the right edge).
    let bg1 = records
        .iter()
        .filter(|r| r.layer == TileLayer::Background(0))
        .count();
    assert_eq!(bg1, 32 * 224);
}
