//! Port of `InputActionTests.cs`: the logical action layer of SPEC-08 §3.1, §3.9, §3.11 and
//! §3.13 — the bit values, the combination rules, the input word, the camera sources and the
//! hotkeys, all driven by injected masks with no window in sight (REQ-INP-120).
#![allow(clippy::expect_used, clippy::unwrap_used)] // test code
#![allow(clippy::float_cmp)] // exact values by construction

mod common;

use common::FakeGamepad;
use rstest::rstest;
use zax_app::emulation::hotkey_tracker::{HotkeyEvents, HotkeyTracker};
use zax_app::input::cursor_auto_hide::CursorAutoHide;
use zax_app::input::default_bindings;
use zax_app::input::input_action::{self, InputAction};
use zax_app::input::libretro_input_map::{self, RetroAnalogAxis, RetroPadButton};
use zax_app::input::menu_navigation::{MenuNavigation, MenuNavigationEvents};
use zax_app::input::routing;
use zax_platform::input::{calibration, standard_gamepad, GamepadDevice};
use zax_render::camera::SceneCamera;
use zax_render::controls::{self, CameraAction};

#[rstest]
// SPEC-08 §4.1: the bit values the Lua scripts see.
#[case(InputAction::A, 0x01)]
#[case(InputAction::B, 0x02)]
#[case(InputAction::SELECT, 0x04)]
#[case(InputAction::START, 0x08)]
#[case(InputAction::UP, 0x10)]
#[case(InputAction::DOWN, 0x20)]
#[case(InputAction::LEFT, 0x40)]
#[case(InputAction::RIGHT, 0x80)]
#[case(InputAction::ZOOM_IN, 0x100)]
#[case(InputAction::ZOOM_OUT, 0x200)]
#[case(InputAction::ROT_LEFT, 0x400)]
#[case(InputAction::ROT_RIGHT, 0x800)]
#[case(InputAction::ROT_UP, 0x1000)]
#[case(InputAction::ROT_DOWN, 0x2000)]
#[case(InputAction::MENU, 0x4000)]
#[case(InputAction::ROLL_BACK, 0x8000)]
#[case(InputAction::FAST_FORWARD, 0x10000)]
#[case(InputAction::QUICK_SAVE, 0x20000)]
#[case(InputAction::QUICK_LOAD, 0x40000)]
fn req_inp_003_the_action_bits_are_those_of_the_table(
    #[case] action: InputAction,
    #[case] bit: u32,
) {
    assert_eq!(action.bits(), bit);
}

/// REQ-INP-001 and REQ-INP-040: exactly 20 actions — the 19 of the table of §4.1 in its
/// display order, then the soft reset AMD-044 REQ-INP-125 adds to the extended actions — and
/// exactly two players.
#[test]
fn req_inp_001_there_are_twenty_actions_in_the_order_of_the_table() {
    assert_eq!(input_action::COUNT, 20);
    assert_eq!(input_action::ALL.len(), input_action::COUNT);
    assert_eq!(input_action::PLAYER_COUNT, 2);
    let distinct: std::collections::BTreeSet<u32> = input_action::ALL
        .iter()
        .map(|action| action.bits())
        .collect();
    assert_eq!(distinct.len(), input_action::ALL.len());

    let names: Vec<&str> = input_action::ALL
        .iter()
        .map(|action| input_action::name_of(*action))
        .collect();
    assert_eq!(
        names,
        [
            "Left",
            "Right",
            "Up",
            "Down",
            "A",
            "B",
            "Select",
            "Start",
            "RotLeft",
            "RotRight",
            "RotUp",
            "RotDown",
            "ZoomIn",
            "ZoomOut",
            "Menu",
            "RollBack",
            "FastForward",
            "QuickSave",
            "QuickLoad",
            "SoftReset",
        ]
    );

    for action in input_action::ALL {
        assert_eq!(
            input_action::parse(input_action::name_of(action)),
            Some(action)
        );
    }
}

/// REQ-INP-002: a player's mask is the OR of every source — the right arrow plus the pad's south
/// button give `0x81`.
#[test]
fn req_inp_002_sources_are_combined_with_a_bitwise_or() {
    let keyboard = InputAction::RIGHT;
    let pad = InputAction::A;
    assert_eq!((keyboard | pad).bits(), 0x81);
    assert_eq!((keyboard | InputAction::NONE).bits(), 0x80);
    assert_eq!((InputAction::NONE | pad).bits(), 0x01);
}

/// The camera actions map onto the renderer's controls.
#[test]
fn req_inp_080_the_camera_actions_map_onto_the_renderer_controls() {
    let actions = input_action::to_camera_actions(
        InputAction::ROT_LEFT | InputAction::ZOOM_OUT | InputAction::A,
    );
    assert_eq!(actions, CameraAction::ROTATE_LEFT | CameraAction::ZOOM_OUT);
}

/// REQ-INP-071 and REQ-INP-072: the script pad is OR-merged into the mask of the player it is
/// attached to and can never take a bit away.
#[test]
fn req_inp_071_the_script_pad_is_merged_into_its_player_only() {
    let merged =
        routing::merge_script_pad(InputAction::RIGHT, InputAction::LEFT, InputAction::START, 0);
    assert_eq!(
        merged,
        (InputAction::RIGHT | InputAction::START, InputAction::LEFT)
    );

    let merged =
        routing::merge_script_pad(InputAction::RIGHT, InputAction::LEFT, InputAction::START, 1);
    assert_eq!(
        merged,
        (InputAction::RIGHT, InputAction::LEFT | InputAction::START)
    );

    // REQ-INP-072: a pad that returns 0 leaves the physical input untouched.
    let merged =
        routing::merge_script_pad(InputAction::RIGHT, InputAction::LEFT, InputAction::NONE, 0);
    assert_eq!(merged, (InputAction::RIGHT, InputAction::LEFT));
}

/// REQ-INP-100 and REQ-INP-105: the plain hotkeys are read on the OR of both players' masks,
/// whatever the port types are — J2's Menu key opens the menu.
#[test]
fn req_inp_100_hotkeys_are_read_on_both_players() {
    let mut tracker = HotkeyTracker::new();
    assert_eq!(
        tracker.update(InputAction::NONE, InputAction::MENU, 0.1),
        HotkeyEvents::NONE
    );
    assert_eq!(
        tracker.update(InputAction::NONE, InputAction::NONE, 0.0),
        HotkeyEvents::MENU_SHORT
    );
}

/// REQ-INP-104: the quick-slot combinations are evaluated player by player, so Select held by J1
/// and the stick flicked by J2 trigger nothing.
#[test]
fn req_inp_104_the_combinations_are_evaluated_player_by_player() {
    let mut split = HotkeyTracker::new();
    split.update(InputAction::SELECT, InputAction::NONE, 0.2);
    split.update(InputAction::SELECT, InputAction::ROT_DOWN, 0.02);
    assert_eq!(
        split.update(InputAction::SELECT, InputAction::NONE, 0.02),
        HotkeyEvents::NONE
    );

    // The same player holding Select and releasing RotDown does save.
    let mut same = HotkeyTracker::new();
    same.update(InputAction::NONE, InputAction::SELECT, 0.2);
    same.update(
        InputAction::NONE,
        InputAction::SELECT | InputAction::ROT_DOWN,
        0.02,
    );
    assert!(same
        .update(InputAction::NONE, InputAction::SELECT, 0.02)
        .contains(HotkeyEvents::QUICK_SAVE));
}

/// REQ-INP-104: Select must have been held for more than 0.1 s before the combination arms, so
/// pressing both together does nothing.
#[test]
fn req_inp_104_the_combination_needs_select_held_for_a_tenth_of_a_second() {
    let mut tracker = HotkeyTracker::new();
    tracker.update_mask(InputAction::SELECT | InputAction::ROT_DOWN, 0.02);
    assert_eq!(
        tracker.update_mask(InputAction::SELECT, 0.02),
        HotkeyEvents::NONE
    );
}

/// REQ-INP-122: the default libretro correspondence of §4.4, with A on the east button and B on
/// the south one, and no hotkey exposed as a button.
#[test]
fn req_inp_122_the_libretro_table_is_the_one_of_section_4_4() {
    use libretro_input_map::{action_of_axis, action_of_button};
    assert_eq!(action_of_button(RetroPadButton::Left), InputAction::LEFT);
    assert_eq!(action_of_button(RetroPadButton::Right), InputAction::RIGHT);
    assert_eq!(action_of_button(RetroPadButton::Up), InputAction::UP);
    assert_eq!(action_of_button(RetroPadButton::Down), InputAction::DOWN);

    // A = east, B = south, the usual libretro NES convention, different from §4.3 on purpose.
    assert_eq!(action_of_button(RetroPadButton::A), InputAction::A);
    assert_eq!(action_of_button(RetroPadButton::B), InputAction::B);
    assert_eq!(
        action_of_button(RetroPadButton::Select),
        InputAction::SELECT
    );
    assert_eq!(action_of_button(RetroPadButton::Start), InputAction::START);
    assert_eq!(action_of_button(RetroPadButton::L2), InputAction::ZOOM_IN);
    assert_eq!(action_of_button(RetroPadButton::R2), InputAction::ZOOM_OUT);

    assert_eq!(
        action_of_axis(RetroAnalogAxis::RightStickLeft),
        InputAction::ROT_LEFT
    );
    assert_eq!(
        action_of_axis(RetroAnalogAxis::RightStickRight),
        InputAction::ROT_RIGHT
    );
    assert_eq!(
        action_of_axis(RetroAnalogAxis::RightStickUp),
        InputAction::ROT_UP
    );
    assert_eq!(
        action_of_axis(RetroAnalogAxis::RightStickDown),
        InputAction::ROT_DOWN
    );

    // The hotkeys belong to the frontend host and are not mapped.
    for (_, action) in libretro_input_map::BUTTONS {
        assert_eq!(
            action & libretro_input_map::HOST_RESERVED_ACTIONS,
            InputAction::NONE
        );
    }
}

/// REQ-INP-105: the directional actions walk the tabs on their rising edge while the menu is
/// open, and are suspended while a binding capture is listening (REQ-INP-047).
#[test]
fn req_inp_105_menu_navigation_reads_rising_edges_only_while_paused() {
    let mut navigation = MenuNavigation::new();

    // Not paused: nothing is produced, and the held key is consumed.
    assert_eq!(
        navigation.update(InputAction::RIGHT, false, false),
        MenuNavigationEvents::NONE
    );
    assert_eq!(
        navigation.update(InputAction::RIGHT, true, false),
        MenuNavigationEvents::NONE
    );

    navigation.update(InputAction::NONE, true, false);
    assert_eq!(
        navigation.update(InputAction::RIGHT, true, false),
        MenuNavigationEvents::NEXT
    );

    // Held down: no repeat.
    assert_eq!(
        navigation.update(InputAction::RIGHT, true, false),
        MenuNavigationEvents::NONE
    );

    // REQ-INP-047: suspended while a capture listens.
    navigation.update(InputAction::NONE, true, false);
    assert_eq!(
        navigation.update(InputAction::RIGHT, true, true),
        MenuNavigationEvents::NONE
    );
}

/// REQ-INP-110 and REQ-INP-111: the cursor is hidden at the twentieth consecutive 0.1 s check
/// without movement, comes back on any movement, and is always visible outside a running game.
#[test]
fn req_inp_110_the_cursor_hides_after_two_still_seconds() {
    let mut cursor = CursorAutoHide::new();

    // 1.9 s still: still visible.
    for _ in 0..19 {
        assert!(cursor.update(true, false, 0.1));
    }
    // The twentieth check hides it (2.0 s).
    assert!(!cursor.update(true, false, 0.1));

    // Any movement shows it again immediately and resets the counter.
    assert!(cursor.update(true, true, 0.1));
    assert_eq!(cursor.still_checks(), 0);

    // REQ-INP-111: outside a running game it is visible whatever the counter says.
    for _ in 0..25 {
        cursor.update(true, false, 0.1);
    }
    assert!(!cursor.cursor_visible());
    assert!(cursor.update(false, false, 0.1));
}

/// REQ-INP-007: there is no turbo and no auto-repeat action in the default list.
#[test]
fn req_inp_007_there_is_no_turbo_action() {
    for action in input_action::ALL {
        let name = input_action::name_of(action).to_ascii_lowercase();
        assert!(
            !name.contains("turbo") && !name.contains("repeat"),
            "{name}"
        );
    }
}

/// REQ-INP-033 and REQ-INP-081: a camera action coming from a stick is digital — the rotation
/// runs at 30 degrees per second once the threshold is crossed, so 50 % and 100 % of travel turn
/// the camera by the same amount.
#[test]
fn req_inp_033_a_stick_turns_the_camera_at_a_constant_speed() {
    fn rotate(travel: f32, seconds: f32) -> f32 {
        let pad = FakeGamepad::new("Pad");
        pad.axis(standard_gamepad::RIGHT_STICK_X, travel);

        let set = default_bindings::create_gamepad(true);
        let mask = set.sample(|control| calibration::is_pressed(pad.value(control)));

        let mut camera = SceneCamera::new();
        camera.reset_view();
        let before = camera.current().angles.y;
        controls::apply(&mut camera, input_action::to_camera_actions(mask), seconds);
        camera.current().angles.y - before
    }

    let half = rotate(0.5, 1.0);
    let full = rotate(1.0, 1.0);
    assert!((half - full).abs() < 1e-4);

    // REQ-INP-081: 3 s of RotRight add 90 degrees +/- 3 to the yaw setpoint.
    assert!((87.0..=93.0).contains(&rotate(1.0, 3.0)));

    // Below the threshold nothing moves at all (REQ-INP-030).
    assert!(rotate(0.2, 1.0).abs() < 1e-4);
}

/// REQ-INP-082: one second of `ZoomOut` adds 70 scene units to the camera distance, and `ZoomIn`
/// wins over `ZoomOut`.
#[test]
fn req_inp_082_zoom_moves_seventy_units_per_second() {
    let mut camera = SceneCamera::new();
    camera.reset_view();
    let start = camera.current().distance();

    controls::apply(&mut camera, CameraAction::ZOOM_OUT, 1.0);
    assert!((67.0..=73.0).contains(&(camera.current().distance() - start)));

    // ZoomIn wins when both are held.
    let held = camera.current().distance();
    controls::apply(
        &mut camera,
        CameraAction::ZOOM_IN | CameraAction::ZOOM_OUT,
        1.0,
    );
    assert!((67.0..=73.0).contains(&(held - camera.current().distance())));
}

