# Porting the Xnes3D+ app to the SNES

The owner's decision (2026-11-18): copy the Xnes3D+ app and edit out what the SNES does not need.

## Done
- Copied from Xnes3D+ (main, 2a04de9): `zax-app`, `zax-platform`, `zax-scripting`,
  `zax-achievements`, `zax-android`, `rust/build` (packaging), the patched
  `librashader-runtime-wgpu`, and the files they include (branding icons, `.github/fork/VERSION`,
  the spec files of `docs/spec`, `rust/docs/EDITOR*.md`, `rust/goldens/lua-api.txt`).
- `zax-nes` is replaced by `zax-snes` in the Cargo files. The `dev-vendor` release bundles every
  crate the app needs, so the workspace builds offline.
- The layer engines and how their tiles are put together (`zax_snes::layers`) are shared by
  `snes-preview` and the app.

## Done in the port
- `cargo check -p zax-app --all-targets` passes.
- `zax-app/src/snes.rs`: `SnesCore` (with stand-ins for the NES app's 2D picture path, memory
  reads for scripts, run-ahead, battery RAM: Mesen keeps its own battery saves). Sound goes
  through a ring that resamples Mesen's 48 kHz to the rate the audio stream asks for.
- `InputAction` has X, Y, L, R; `routing::core_word` gives a `PadWord` (player 1 low half,
  player 2 high half, Mesen's bit order).
- `GameSession`: its engine runs layer 0, `LayerEngines` the others, `build_draw_list` merges;
  `exchange_draw_list` swaps the merged list.
- Achievements: rcheevos is told the console is a SNES and hashes the ROM file itself.
  Memory reads for achievements still give 0 (see "Still to do").
- The MCP tools: the movie tools are gone; `dump_frame` writes the tiles layer by layer with
  the 256 finished colours.
- Removed: movies (modules, commands, menus, movie bar, note dialog, MCP tools), hacks and
  widescreen (host modules, compat cache, handheld and desktop hacks screens, game-options
  rows), the Zapper, the demo game and its editor example, port types (commands and menus).
- Controls: X, Y, L and R are bound by default (keyboard S, A, Q, W; pads in SNES places, L and
  R on the shoulders). On Android, X and Y are no longer turbo A and B, and L1/R1 no longer
  quick load and save (those are in the menu). In a game the handheld interface leaves X and Y
  alone (the NES app used them for the default view and the 2D view).
- Lua: Mesen's own copy of Lua is not built; Mesen links the Lua of `mlua-sys`, the one the
  app's scripts use, because two copies clash when the app is linked. Mesen's watchdog hook is a
  stub in the shim (Mesen's script engine never runs).
- Tests: the NES-only test files are deleted (movies, hacks, Zapper, run-ahead, CHR-RAM, the
  NES bus). The test ROM of the others is `tests/roms/hello.sfc`. They run with
  `--test-threads=1`, since only one Mesen may run at a time.

## Still to do
- App tests: 681 pass, 41 fail (run with `--test-threads=1`; on this machine link SDL2 through
  `LIBRARY_PATH`). The failures are goldens and counts that the SNES buttons and the removed
  NES pages changed, tests that read RAM through `ReadMem` (0 for now), battery saves (Mesen
  keeps its own), and a few the NES app's own copy already failed (rollback capacity). Still
  failing:
  - `a_failed_snapshot_leaves_the_history_as_it_was`
  - `a_handheld_pad_gets_the_handheld_defaults`
  - `a_saved_desktop_default_pad_moves_to_the_handheld_defaults`
  - `a_script_reading_memory_sees_the_same_bytes_ahead`
  - `audio_2_both_outputs_pump_and_correct_without_a_device`
  - `nothing_of_the_3d_runs_in_2d_while_it_is_paused`
  - `quit_to_the_rom_list_stops_the_game`
  - `req_inp_001_there_are_twenty_actions_in_the_order_of_the_table`
  - `req_inp_010_a_key_can_drive_both_players`
  - `req_inp_020_the_default_gamepad_set_is_the_table_of_section_4_3`
  - `req_inp_025_gamepad_bindings_belong_to_the_device_identity`
  - `req_inp_049_a_set_can_be_reset_to_its_defaults`
  - `req_inp_125_the_bound_action_triggers_the_soft_reset`
  - `req_loop_022_a_zip_uses_its_first_entry`
  - `req_loop_030_the_history_keeps_thirty_snapshots`
  - `req_loop_032_a_rollback_restores_the_second_to_last_snapshot`
  - `req_loop_072_a_soft_reset_keeps_the_learned_conversion_state`
  - `req_loop_072_a_soft_reset_keeps_the_profile_and_empties_the_history`
  - `req_loop_072_a_soft_reset_keeps_the_ram_where_a_hard_reset_clears_it`
  - `req_loop_076_quitting_during_a_game_offers_the_resume_point_at_the_next_launch`
  - `req_pers_060_the_battery_ram_survives_a_restart`
  - `req_pub_001_the_library_without_an_asset_folder_holds_the_two_generic_entries`
  - `req_pub_002_the_repository_carries_no_asset_file`
  - `req_ui_082_the_in_game_notifications_have_their_exact_texts`
  - `req_ui_203_the_window_lists_the_actions_of_both_players_and_the_commands`
  - `ro_1_the_desktop_window_shows_exactly_the_settings_it_showed_before`
  - `ro_3_every_row_the_console_offered_is_still_reachable`
  - `ro_3_the_advanced_options_of_a_game_are_a_hub_of_pages`
  - `ro_3_the_handheld_main_pages_are_the_mockups`
  - `ro_3_the_option_tree_matches_its_golden`
  - `the_2d_picture_is_seen_head_on_and_leaves_the_camera_alone`
  - `the_load_screen_loads_a_state_and_leaves_an_empty_slot_alone`
  - `the_options_change_the_settings_of_the_host`
  - `the_options_of_a_game_hold_only_what_changes_and_play_with_it`
  - `the_saved_token_signs_in_and_a_frame_unlocks_in_softcore`
  - `ui::handheld::options::tests::ro_3_a_group_title_is_never_selected`
  - `ui::handheld::overlay::tests::the_slot_steps_on_the_state_rows_only_and_wraps`
  - `ui::handheld::states::tests::it_opens_on_the_slot_of_the_menu_and_wraps`
  - `ui::handheld::states::tests::left_and_right_move_by_a_page_without_wrapping`
  - `ui::platform::tests::the_standard_buttons_reach_the_handheld_buttons_and_the_imgui_keys`
  - `x_chooses_the_profile_and_a_plays_with_it`

- Done since: the 2D picture is RGBA (four bytes a pixel, alpha 0 not drawn); `ReadMem` and
  `WriteMem` reach the first 64 KiB of work RAM (bank $7E), achievements the RetroAchievements
  SNES map; visible "Zaxnes" text, community profiles and 3dSen import are gone; the SNES look;
  the Android APK workflow (`android-apk.yml`, a pre-release named after `.github/fork/VERSION`).
  v0.1.0 test build released 2026-11-18, not yet tried on a device.
- Leftover NES settings (widescreen, region, the NES palette option).
- Controls: check the labels on the controls pages.
- Look: XSnes3D+ names, SNES purple, grey and black with VT323.
- Desktop build, then the Android package.

## Plan
1. `crate::snes` in `zax-app`: the SNES core as the app sees it (`SnesCore`, wrapping
   `zax_snes::Snes`): load from bytes (written to a file in the home folder), run a frame
   with both pads, the picture (RGB), the layer input of the frame, sound through an
   `AudioReader` ring, savestates, reset, battery saves (Mesen writes them itself).
2. `GameSession`: its own engine runs layer 0 (with the profile's scripts); a
   `zax_snes::layers::LayerEngines` runs the other layers; the draw list merges them.
3. Cut, in this order, what only the NES has: movies (FCEUX), widescreen hacks and NES patch
   packs, the Zapper, port types, CHR-RAM tile offers, run-ahead (needs cheap savestates;
   may come back), memory snapshots for `ReadMem`.
4. The renderer's 2D picture is NES colour indices; the SNES needs an RGB picture path.
5. Achievements: SNES hashing later.
6. Names and look: XSnes3D+ titles, SNES purple/grey/black with VT323.

## Rules kept from Xnes3D+
- Commits "Uploaded by Claude", plain comments, no em dashes.
- CI does not build `zax-app` yet (only `-p` the SNES crates), so work in progress can land.
