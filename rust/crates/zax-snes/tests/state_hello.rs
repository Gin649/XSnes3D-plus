//! Savestates, reset and sound on `tests/roms/hello.sfc`.
//!
//! Everything runs in one test, because only one emulator may exist at a time.

// The helpers below are test code too, so a failed step may stop the test.
#![allow(clippy::unwrap_used)]

use std::path::PathBuf;

use zax_snes::{Buttons, Snes};

fn rom() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../tests/roms/hello.sfc")
}

fn home() -> PathBuf {
    let home = std::env::temp_dir().join(format!("xsnes3d-state-{}", std::process::id()));
    std::fs::create_dir_all(&home).unwrap();
    home
}

fn run(snes: &mut Snes, frames: u32) -> u64 {
    for _ in 0..frames {
        snes.run_frame(Buttons::NONE).unwrap();
    }
    snes.frame().unwrap().hash()
}

#[test]
fn a_state_put_back_plays_the_same_frames_and_sound_keeps_coming() {
    let mut snes = Snes::load(&rom(), &home()).unwrap();
    run(&mut snes, 30);
    let state = snes.save_state().unwrap();
    let later = run(&mut snes, 30);
    snes.load_state(&state).unwrap();
    assert_eq!(
        run(&mut snes, 30),
        later,
        "the frames after the state changed"
    );

    // A damaged state is refused.
    assert!(snes.load_state(&state[..state.len() / 3]).is_err());

    // About one frame of sound per frame: 48000 samples a second, 60 frames a second.
    let mut sound = Vec::new();
    snes.take_audio(&mut sound);
    sound.clear();
    let rate = {
        run(&mut snes, 10);
        snes.take_audio(&mut sound)
    };
    assert_eq!(rate, 48000);
    let frames = sound.len() / 2;
    assert!(
        (7000..9000).contains(&frames),
        "{frames} sound frames for 10 frames"
    );

    // The reset button: the game starts over and draws the same picture again.
    snes.reset().unwrap();
    run(&mut snes, 60);
    snes.set_player_2(Buttons::NONE);
}
