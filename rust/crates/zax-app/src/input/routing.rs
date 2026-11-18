//! How the two players' actions reach the core and the camera (SPEC-08 §4.1).

use crate::snes::PadWord;

use super::input_action::{pad_bits, InputAction};

/// The word `SnesCore::run_frame` takes: player 1's pad in the low half, player 2's in the
/// high one.
#[must_use]
pub fn core_word(player_one: InputAction, player_two: InputAction) -> PadWord {
    PadWord::from(pad_bits(player_one)) | (PadWord::from(pad_bits(player_two)) << 16)
}

/// The camera actions of both players.
#[must_use]
pub fn camera_sources(player_one: InputAction, player_two: InputAction) -> InputAction {
    (player_one | player_two) & InputAction::CAMERA_MASK
}

/// Adds the script pad's mask to the player it is attached to, never masking a physical input
/// (REQ-INP-071, REQ-INP-072).
#[must_use]
pub fn merge_script_pad(
    player_one: InputAction,
    player_two: InputAction,
    pad_mask: InputAction,
    pad_index: i32,
) -> (InputAction, InputAction) {
    if pad_index == 0 {
        (player_one | pad_mask, player_two)
    } else {
        (player_one, player_two | pad_mask)
    }
}

#[cfg(test)]
mod tests {
    use super::{core_word, InputAction};

    #[test]
    fn each_player_has_half_of_the_word() {
        let word = core_word(InputAction::A | InputAction::R, InputAction::RIGHT);
        assert_eq!(word, 0b10_0001 | (0b1000_0000_0000 << 16));
    }
}
