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
- Run the app tests and fix what the SNES core changes (some read RAM through `ReadMem`, which
  gives 0 for now).
- The renderer's 2D picture path for RGB (the 2D view is blank until then).
- A SNES memory map for `ReadMem`, `WriteMem` and achievements (work RAM, then save RAM).
- Leftover NES settings and words (widescreen, region, port menus, "NES" in strings).
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
