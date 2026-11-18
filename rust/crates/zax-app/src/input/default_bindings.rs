//! The default bindings (SPEC-08 §4.2, §4.3).

use zax_platform::input::{standard_gamepad, Key, PhysicalControl};

use super::binding_set::BindingSet;
use super::binding_target::BindingDeviceKind;
use super::input_action::InputAction;

/// Player one's default keys. `QuickSave` and `QuickLoad` have no default key: the combinations of
/// REQ-INP-104 are the only default route to the quick slot (SPEC-08 §4.2, §4.3).
pub const PLAYER_ONE_KEYBOARD: [(InputAction, Key); 21] = [
    (InputAction::LEFT, Key::Left),
    (InputAction::RIGHT, Key::Right),
    (InputAction::UP, Key::Up),
    (InputAction::DOWN, Key::Down),
    (InputAction::A, Key::X),
    (InputAction::B, Key::Z),
    (InputAction::X, Key::S),
    (InputAction::Y, Key::A),
    (InputAction::L, Key::Q),
    (InputAction::R, Key::W),
    (InputAction::SELECT, Key::Space),
    (InputAction::START, Key::Enter),
    (InputAction::ROT_LEFT, Key::F5),
    (InputAction::ROT_RIGHT, Key::F6),
    (InputAction::ROT_UP, Key::F7),
    (InputAction::ROT_DOWN, Key::F8),
    (InputAction::ZOOM_IN, Key::F9),
    (InputAction::ZOOM_OUT, Key::F10),
    (InputAction::MENU, Key::Escape),
    (InputAction::ROLL_BACK, Key::Comma),
    (InputAction::FAST_FORWARD, Key::Period),
];

/// The defaults of a gamepad that answers to the standard numbering (SPEC-08 §4.3), with the
/// SNES buttons where a SNES pad has them: B bottom, A right, Y left, X top, L and R on the
/// shoulders. Fast forward is on the left stick's click; rollback has no button by default.
#[must_use]
pub fn standard_gamepad_set() -> [(InputAction, PhysicalControl); 20] {
    [
        (
            InputAction::LEFT,
            PhysicalControl::button(standard_gamepad::D_PAD_LEFT),
        ),
        (
            InputAction::RIGHT,
            PhysicalControl::button(standard_gamepad::D_PAD_RIGHT),
        ),
        (
            InputAction::UP,
            PhysicalControl::button(standard_gamepad::D_PAD_UP),
        ),
        (
            InputAction::DOWN,
            PhysicalControl::button(standard_gamepad::D_PAD_DOWN),
        ),
        (
            InputAction::A,
            PhysicalControl::button(standard_gamepad::EAST),
        ),
        (
            InputAction::B,
            PhysicalControl::button(standard_gamepad::SOUTH),
        ),
        (
            InputAction::X,
            PhysicalControl::button(standard_gamepad::NORTH),
        ),
        (
            InputAction::Y,
            PhysicalControl::button(standard_gamepad::WEST),
        ),
        (
            InputAction::L,
            PhysicalControl::button(standard_gamepad::LEFT_BUMPER),
        ),
        (
            InputAction::R,
            PhysicalControl::button(standard_gamepad::RIGHT_BUMPER),
        ),
        (
            InputAction::SELECT,
            PhysicalControl::button(standard_gamepad::BACK),
        ),
        (
            InputAction::START,
            PhysicalControl::button(standard_gamepad::START),
        ),
        (
            InputAction::FAST_FORWARD,
            PhysicalControl::button(standard_gamepad::LEFT_STICK),
        ),
        (
            InputAction::ROT_LEFT,
            PhysicalControl::axis_negative(standard_gamepad::RIGHT_STICK_X),
        ),
        (
            InputAction::ROT_RIGHT,
            PhysicalControl::axis_positive(standard_gamepad::RIGHT_STICK_X),
        ),
        (
            InputAction::ROT_UP,
            PhysicalControl::axis_positive(standard_gamepad::RIGHT_STICK_Y),
        ),
        (
            InputAction::ROT_DOWN,
            PhysicalControl::axis_negative(standard_gamepad::RIGHT_STICK_Y),
        ),
        (
            InputAction::ZOOM_IN,
            PhysicalControl::trigger(standard_gamepad::LEFT_TRIGGER),
        ),
        (
            InputAction::ZOOM_OUT,
            PhysicalControl::trigger(standard_gamepad::RIGHT_TRIGGER),
        ),
        (
            InputAction::MENU,
            PhysicalControl::button(standard_gamepad::RIGHT_STICK),
        ),
    ]
}

/// Player one's keyboard: the defaults of §4.2.
#[must_use]
pub fn create_player_one_keyboard() -> BindingSet {
    let mut set = BindingSet::new(BindingDeviceKind::Keyboard);
    for (action, key) in PLAYER_ONE_KEYBOARD {
        set.set_action(action, PhysicalControl::key(key.code()));
    }
    set
}

/// Player two's keyboard: nothing bound.
#[must_use]
pub const fn create_player_two_keyboard() -> BindingSet {
    BindingSet::new(BindingDeviceKind::Keyboard)
}

/// The keyboard of `player`: 0 for player one, anything else for player two.
#[must_use]
pub fn create_keyboard(player: usize) -> BindingSet {
    if player == 0 {
        create_player_one_keyboard()
    } else {
        create_player_two_keyboard()
    }
}

/// The defaults of a handheld console's pad (Rust-only, owner request), whose face buttons the
/// platform names by their labels (A right, B bottom, X top, Y left): the SNES buttons on the
/// buttons of the same name, L and R on the shoulders, fast forward on R2. L2 is the handheld
/// interface's (camera memory); zoom has no button by default (a pinch of the screen); no rewind
/// on a handheld.
#[must_use]
pub fn handheld_gamepad_set() -> [(InputAction, PhysicalControl); 17] {
    [
        (
            InputAction::LEFT,
            PhysicalControl::button(standard_gamepad::D_PAD_LEFT),
        ),
        (
            InputAction::RIGHT,
            PhysicalControl::button(standard_gamepad::D_PAD_RIGHT),
        ),
        (
            InputAction::UP,
            PhysicalControl::button(standard_gamepad::D_PAD_UP),
        ),
        (
            InputAction::DOWN,
            PhysicalControl::button(standard_gamepad::D_PAD_DOWN),
        ),
        (
            InputAction::A,
            PhysicalControl::button(standard_gamepad::SOUTH),
        ),
        (
            InputAction::B,
            PhysicalControl::button(standard_gamepad::EAST),
        ),
        (
            InputAction::X,
            PhysicalControl::button(standard_gamepad::NORTH),
        ),
        (
            InputAction::Y,
            PhysicalControl::button(standard_gamepad::WEST),
        ),
        (
            InputAction::L,
            PhysicalControl::button(standard_gamepad::LEFT_BUMPER),
        ),
        (
            InputAction::R,
            PhysicalControl::button(standard_gamepad::RIGHT_BUMPER),
        ),
        (
            InputAction::SELECT,
            PhysicalControl::button(standard_gamepad::BACK),
        ),
        (
            InputAction::START,
            PhysicalControl::button(standard_gamepad::START),
        ),
        (
            InputAction::FAST_FORWARD,
            PhysicalControl::trigger(standard_gamepad::RIGHT_TRIGGER),
        ),
        (
            InputAction::ROT_LEFT,
            PhysicalControl::axis_negative(standard_gamepad::RIGHT_STICK_X),
        ),
        (
            InputAction::ROT_RIGHT,
            PhysicalControl::axis_positive(standard_gamepad::RIGHT_STICK_X),
        ),
        (
            InputAction::ROT_UP,
            PhysicalControl::axis_positive(standard_gamepad::RIGHT_STICK_Y),
        ),
        (
            InputAction::ROT_DOWN,
            PhysicalControl::axis_negative(standard_gamepad::RIGHT_STICK_Y),
        ),
    ]
}

/// A gamepad set: the standard defaults when `standard`, otherwise empty (a pad without a
/// mapping reports raw indices, REQ-INP-021).
#[must_use]
pub fn create_gamepad(standard: bool) -> BindingSet {
    let mut set = BindingSet::new(BindingDeviceKind::Gamepad);
    if standard {
        for (action, control) in standard_gamepad_set() {
            set.set_action(action, control);
        }
    }
    set
}

/// The gamepad set of the desktop, which plays as its console mode (Rust-only, owner request):
/// the handheld defaults with the face buttons where a handheld has them, not by their labels,
/// as Mesen has them: SNES A on the right button (Xbox B), SNES B on the bottom one (Xbox A). The
/// rollback, which a handheld has not, goes on the right stick's click.
#[must_use]
pub fn create_desktop_console_gamepad(standard: bool) -> BindingSet {
    let mut set = create_former_desktop_console_gamepad(standard);
    if standard {
        set.set_action(
            InputAction::ROLL_BACK,
            PhysicalControl::button(standard_gamepad::RIGHT_STICK),
        );
    }
    set
}

/// The desktop's console set before the rollback had a button: what a pad saved then still has.
#[must_use]
pub fn create_former_desktop_console_gamepad(standard: bool) -> BindingSet {
    let mut set = create_handheld_gamepad(standard);
    if standard {
        set.set_action(
            InputAction::A,
            PhysicalControl::button(standard_gamepad::EAST),
        );
        set.set_action(
            InputAction::B,
            PhysicalControl::button(standard_gamepad::SOUTH),
        );
    }
    set
}

/// The gamepad set of a handheld console: the handheld defaults when `standard`.
#[must_use]
pub fn create_handheld_gamepad(standard: bool) -> BindingSet {
    let mut set = BindingSet::new(BindingDeviceKind::Gamepad);
    if standard {
        for (action, control) in handheld_gamepad_set() {
            set.set_action(action, control);
        }
    }
    set
}

/// The gamepad set the Android package had before Xnes3D+ changed it: what a pad saved then
/// still has, so it moves to the new one ([`create_android_gamepad`]).
#[must_use]
pub fn create_former_android_gamepad(standard: bool) -> BindingSet {
    let mut set = create_handheld_gamepad(standard);
    if standard {
        set.set_action(
            InputAction::ROLL_BACK,
            PhysicalControl::button(standard_gamepad::RIGHT_STICK),
        );
    }
    set
}

/// The gamepad set of the Android package: the handheld defaults, with the face buttons where
/// the handheld has them (SNES A on the right button, which the AYN Thor labels A and reports as
/// the pad's east one; SNES B on the bottom one; X on top, Y on the left), L and R on the
/// shoulders, and the triggers as RetroArch players expect them: L2 rewinds (held), R2 fast
/// forwards. Quick save and load are in the menu. Zoom stays on a pinch of the screen, and both
/// sticks pressed in open the menu (the handheld interface, not a binding).
#[must_use]
pub fn create_android_gamepad(standard: bool) -> BindingSet {
    let mut set = create_android_gamepad_by_labels(standard);
    if standard {
        set.set_action(
            InputAction::A,
            PhysicalControl::button(standard_gamepad::EAST),
        );
        set.set_action(
            InputAction::B,
            PhysicalControl::button(standard_gamepad::SOUTH),
        );
    }
    set
}

/// The Android set with SNES A and B on the pad's A and B labels, which the AYN Thor swaps (the
/// Xnes3D+ set up to its v1.0.0, kept so a saved pad of that shape moves to
/// [`create_android_gamepad`]).
#[must_use]
pub fn create_android_gamepad_by_labels(standard: bool) -> BindingSet {
    let mut set = create_handheld_gamepad(standard);
    if standard {
        set.set_action(
            InputAction::ROLL_BACK,
            PhysicalControl::trigger(standard_gamepad::LEFT_TRIGGER),
        );
    }
    set
}
