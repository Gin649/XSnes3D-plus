//! Rebuilds whole tiles from the hello ROM's recorded rows, checks them, and hands each layer
//! to its own 3D engine.

// The helpers below are test code too, so a failed step may stop the test.
#![allow(clippy::unwrap_used)]

use std::path::PathBuf;

use zax_engine3d::engine::ConversionEngine;
use zax_engine3d::palette::Rgb24;
use zax_profiles::profile::GameProfile;
use zax_snes::{Buttons, Snes};

const SOLID: u64 = u64::MAX;

#[test]
fn hello_tiles_reach_one_engine_per_layer() {
    let rom = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../tests/roms/hello.sfc");
    let home = std::env::temp_dir().join(format!("xsnes3d-engine-{}", std::process::id()));
    std::fs::create_dir_all(&home).unwrap();
    let mut snes = Snes::load(&rom, &home).unwrap();
    snes.set_tile_tap(true).unwrap();
    for _ in 0..60 {
        snes.run_frame(Buttons::NONE).unwrap();
    }
    let input = snes.tile_capture().unwrap().engine_input();
    drop(snes);

    assert_eq!(input.bg_mode, 1);
    // Mode 1 with backgrounds 1 and 3 and sprites on the main screen.
    let layers: Vec<u8> = input.layers.iter().map(|l| l.layer).collect();
    assert_eq!(layers, vec![0, 2]);
    // The palette: backdrop, yellow, green (BG3 palette 1), red (sprite palette 0).
    assert_eq!(input.colors[1], Rgb24::new(255, 255, 0));
    assert_eq!(input.colors[5], Rgb24::new(0, 255, 0));
    assert_eq!(input.colors[129], Rgb24::new(255, 0, 0));

    let bg1 = &input.layers[0];
    let sprites: Vec<_> = bg1.records.iter().filter(|r| !r.background).collect();
    let tiles: Vec<_> = bg1.records.iter().filter(|r| r.background).collect();
    // 28 rows of 32 tiles, every row of each tile filled.
    assert_eq!(tiles.len(), 28 * 32);
    // The rectangle's corner: tile 1, colour 1 everywhere (plane 0 set, 4 bits per pixel).
    let corner = tiles.iter().find(|r| (r.x, r.y) == (32, 32)).unwrap();
    assert_eq!(
        (corner.plane0, corner.plane1, corner.upper),
        (SOLID, 0, [0; 6])
    );
    assert_eq!(corner.palette, 0);
    assert_eq!(corner.source_pixel(3, 5), Some(1));
    // A blank tile next to it.
    let blank = tiles.iter().find(|r| (r.x, r.y) == (24, 32)).unwrap();
    assert!(blank.is_empty());
    // The sprite: one whole 8x8 tile at 120, 52, palette 0 of the sprites (entry 128).
    assert_eq!(sprites.len(), 1);
    let sprite = sprites[0];
    assert_eq!((sprite.x, sprite.y), (120, 52));
    assert_eq!((sprite.plane0, sprite.palette), (SOLID, 128));

    // Background 3: the band, palette 1 of 4 colours (entries 4 to 7).
    let bg3 = &input.layers[1];
    let band = bg3.records.iter().find(|r| (r.x, r.y) == (0, 48)).unwrap();
    assert_eq!((band.plane0, band.palette), (SOLID, 4));
    assert!(bg3.records.iter().all(|r| r.background));

    // Each layer converts in its own engine, with the frame's 256 colours.
    for layer in &input.layers {
        let mut engine = ConversionEngine::new();
        engine
            .load_profile(&GameProfile::create_empty(), true)
            .unwrap();
        let frame = engine.convert(&input.batch(layer), 0);
        assert_eq!(frame.palette().get(5), Some(Rgb24::new(0, 255, 0)));
        assert!(
            frame.background_tiles().count() > 0,
            "layer {}",
            layer.layer
        );
    }
}
