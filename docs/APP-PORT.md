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

## In progress (getting `cargo check -p zax-app` to pass)
- `zax-app/src/snes.rs`: `SnesCore` (with stand-ins for the NES app's 2D picture path, memory
  reads for scripts, run-ahead, battery RAM: Mesen keeps its own battery saves).
- `InputAction` has X, Y, L, R; `routing::core_word` gives a `PadWord` (player 1 low half,
  player 2 high half, Mesen's bit order).
- `GameSession`: its engine runs layer 0, `LayerEngines` the others, `build_draw_list` merges;
  `exchange_draw_list` swaps the merged list.
- Removed: movies (modules, commands, menus, movie bar, note dialog), hacks/widescreen (host
  modules, compat cache, handheld and desktop hacks screens, game-options rows), the Zapper,
  the demo game, port types (commands).
- Next: the handheld UI's widescreen/hacks parts (`ui/handheld/mod.rs`, `draw.rs`,
  `rom_list`), `script_reload.rs`, `save_states.rs`, `application.rs`, `mcp/tools.rs`,
  `app_settings.rs` (NES region), `audio/stream.rs`, achievements (ROM hashing).

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
