# XSnes3D+: progress and decisions

Last updated 2026-11-18 (night). Kept in step with the claude.ai project doc of the same name.

## Status
- **Android app:** test builds v0.1.0 to v0.1.3 released as pre-releases on Gin649/XSnes3D-plus (`android-apk.yml`, run from the Actions tab; the version is `.github/fork/VERSION`, raise it before each build). Installs as `io.github.gin649.xsnes3d`, beside Xnes3D+.
- **First Thor test (v0.1.0):** runs, but choppy, with sound pops (the sound buffer running dry about twice a second); auto save states said "corrupted" (fixed in v0.1.1: states now carry the game); Yogi Bear crashed (not reproduced yet: its ROM is not in the roms repo).
- **v0.1.1:** save state fix, a "2D" choice in the in-game menu's Camera row, layer conversion on several threads, the sound's rate nudged to keep its buffer steady.
- **v0.1.2:** play reports. **v0.1.3:** "Report this moment". Both work on the Thor (first reports 2026-11-18).
- **First reports (v0.1.3, Yogi Bear):** the SNES emulation alone took 11 to 14 ms per game frame on the Thor; once a frame came late, the catch-up ran a second frame on the main thread and the game stayed stuck near 20 fps (sound running dry hundreds of times). The left 8 columns showed half-drawn scenery in 3D (the game blacks them out with the colour window). The app then crashed with no Rust panic (a native crash or the system closing it). Reports said "0.1.0" and named the game "Smart 3D (no profile)".
- **Fixes after the first reports (next build, untested on the device):** late ticks in a row no longer catch up on the main thread (one late tick still may); the emulation thread is kept on the chip's fastest core and the main thread on the other fast ones; Android draws 60 frames a second by default ("Render Rate" 60fps swaps every other refresh on the 120 Hz screen; 120fps can still be picked); pixels the colour window turns black are hidden in 3D (patch 0004, checked on the Yogi Bear moment with `snes-preview`); a native crash writes a `== NATIVE CRASH` line with the signal and the crash place (offsets into the app's library) to the play report; reports carry the real version and the ROM's name for games without a profile.
- **Open from the first reports:** Aero the Acro-Bat's copyright screen shows wrong letters in 3D (its ROM is not in the roms repo yet); two launches right after the crash refused a save state ("the state is too short").

## How fixes work from now on (the loop)
- **Play reports:** each game played writes `reports/<time>-<game>.txt` on the device (build, device, drivers, stage times and audio counters every 2 s from the always-on profiler, a line every 600 frames with the mode and tiles per layer, Rust panics with a backtrace; a report left open at the next start is marked "the app ended during this game"). With a GitHub token set (Options > Games and 3D profiles > Play reports: a fine-grained token, Contents read/write on `zaxnes-plus-roms-` only), they upload to `reports/<device>/` in Gin649/zaxnes-plus-roms-.
- **Report this moment** (in-game menu): `reports/<device>/moments/<time>-<game>/` with `state.bin` (no ROM), `shot-3d.png`, `shot-2d.png`, `info.txt`.
- **Reproducing a moment:** the game's ROM from the roms repo, then `snes-preview <rom> --state state.bin --frames 2 --shots 1 --out DIR`.
- **Fixes:** general rules first (they help every game using the same trick); a per-game profile only for a game that does something only it does. Every moment received becomes a test case, re-rendered before a change ships.
- **Context:** this file, `CLAUDE.md` and `docs/APP-PORT.md` carry what a new session needs (a new chat in the claude.ai project, or Claude Code on Gin649's PC with the Thor over USB).

## Decisions (app)
- The app is the Xnes3D+ app copied and trimmed for SNES (movies, hacks, widescreen, Zapper, demo, port menus, community profiles and 3dSen import removed). Internal `zax-`/Zaxnes names are kept (renaming is cosmetic); every visible "Zaxnes" is gone; `CREDITS.md` credits Mesen 2 and Zaxnes (GPL-3.0).
- No desktop build is made; Android is the target.
- Look: VT323, SNES purple (A/B), lavender (X/Y), console greys, blacks; "XSNES3D+" logo; icon drawn by `.github/fork/icon.py`.
- Controls: SNES buttons on the pad's buttons of the same name; L/R on the shoulders; L2 rewind, R2 fast forward on Android; quick save/load in the menu.
- Mesen's own copy of Lua is not built; Mesen links the app's Lua (mlua-sys), with a stub for Mesen's watchdog.
- Scripts' ReadMem/WriteMem reach the first 64 KiB of work RAM (bank $7E); achievements use the RetroAchievements SNES map.
- The 2D picture is RGBA, 256 x 240 (hi-res halved).

## Prototype steps (done)
Boot (Mesen 2.1.1), tile tap proven pixel-exact, tiles fed to the 3D engine, 3D pictures in the preview, each background layer at its own depth, Smart 3D started, SNES game list with button scripts for 5 test games.

## Decisions (3D)
- **One 3D engine per SNES background layer** (plus background 3's front tiles and the sky). Sprites go with layer 1. Layers ordered by the SNES priority table, 16 units apart, each grown so the front view matches the console picture.
- **Engine colours widened:** tiles up to 8 bit planes, a 256-entry RGB frame palette.
- **Mesen is changed only through patches** in `rust/crates/zax-snes/patches/` (0001 frame stepping; 0002 tile tap; 0003 Mode 7 tap; 0004 pixels the colour window turns black).
- **Mode 7:** lines cut into 8x8 squares; a road seen at an angle is laid as a floor (up to 3 times the nearest line's distance), a map seen from above stays flat.
- **Windows, colours mid-frame, colour maths, sky gradients:** see the code docs in `zax-snes` (pixels a window hides are dropped; mid-frame palette changes remapped; each palette entry takes its finished colour; per-line sky wall).
- **Clip to black:** pixels the colour window turns black (and colour maths leaves black) are hidden on every layer and sprite; games hide their scroll seam this way.
- **Engine memory cap:** an engine holding more than 3,000 tile images starts afresh.
- **Camera** 0.85 of the NES app's distance, aimed at the 224-line picture's middle; clipping shows all 256 x 224.

## Decisions (performance, Android)
- **Catch-up with the emulation thread:** a late tick may run a second frame once; while ticks keep coming late, only one frame runs per tick and the rest is dropped.
- **Cores:** the emulation thread on the fastest core, the main thread on the other fast cores (`zax-app/src/cpu_affinity.rs`, read from sysfs; nothing changes on a chip whose cores are all alike). The start-up lines of a report say which.
- **Drawing rate:** Android defaults to 60fps; with vsync on, a frame rate at half the screen's refresh or less swaps every other refresh.

## Where things are
- Code: `Gin649/XSnes3D-plus` (`docs/APP-PORT.md`: the app port, what was removed, test status). Tools: `snes-boot`, `snes-preview`.
- ROMs, chip firmware, `snes-games.json`, and the play reports: the private roms repo.

## Known limits (for later)
- Performance: on the Thor (first report, before the core pinning) the emulation took 11 to 14 ms per game frame, the 3D conversion about 5 ms, the scene about 7 ms. On a desktop: emulation about 4 ms, tile capture 4 to 5 ms, layer conversion about 1 ms per frame.
- The Mode 7 floor's far rows are coarse; Super FX 3D (Star Fox) stays flat; Mario Kart's lap number missing in 3D.
- About 41 of the app's own tests still fail (goldens, NES-era cases); listed in `docs/APP-PORT.md`. CI runs the SNES crates and the renderer, and is green. CI does not build `zax-app`; it is checked in the session (below).
- **Building in the session:** the `dev-vendor` release's crates (`.cargo/config.toml` pointing at them, not committed), the stable toolchain (`RUSTUP_TOOLCHAIN=stable`), and for the app's tests `LIBRARY_PATH` at a folder with a `libSDL2.so` link to the system's `libSDL2-2.0.so.0`. The Android-only code (core pinning, crash catcher) is only compiled by the APK build.
