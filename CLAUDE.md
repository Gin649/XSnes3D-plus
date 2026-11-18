# XSnes3D+: notes for Claude

XSnes3D+ is Gin649's 3D SNES emulator for Android (AYN Thor): Mesen 2's SNES core, with each
frame's tiles turned into 3D layers by an engine copied from Xnes3D+ (itself built on Zaxnes).
It is its own app; Zaxnes and Mesen are credited in `CREDITS.md`.

Read first: `docs/PROGRESS.md` (status, decisions, how fixes are made) and `docs/APP-PORT.md`
(the Android app: what was ported, removed, and what still fails).

## How Gin649 wants the work done
- Commit messages are exactly "Uploaded by Claude", author `Claude <noreply@anthropic.com>`,
  with no co-author or session lines.
- Before a big change, show the plan and wait for the go-ahead. Once a plan is approved, go
  ahead without re-checking.
- Explain in broad strokes and plain terms, not every technical step.
- Say plainly what was and wasn't tested.
- Don't start an APK build unless asked. Give a time estimate before a build. When it ends,
  just report success or failure and the release name.
- Don't sit idle waiting on checks or builds: do other work meanwhile. Don't block in polling
  loops. Scheduled checks can be set up without asking.
- Code comments in plain language, no em dashes.
- Change CHANGELOG only with the go-ahead, and only for changes a player would notice.

## Repositories
- `Gin649/XSnes3D-plus`: this code. `android-apk.yml` (Actions tab) builds the APK and
  publishes a pre-release named after `.github/fork/VERSION` (raise it first).
- `Gin649/zaxnes-plus-roms-` (private): test ROMs (`roms/snes/`), chip firmware,
  `snes-games.json`, and the play reports and moments the app uploads (`reports/<device>/`).

## Testing on the Thor (USB debugging is on)
- `adb install -r <apk>` installs over the last build and keeps saves.
- The app's package is `io.github.gin649.xsnes3d`; its files (reports, saves, logs) are under
  `/sdcard/Android/data/io.github.gin649.xsnes3d/files/`.
- `adb logcat` shows crashes, including Mesen's C++ ones that the play reports cannot explain.
- A reported moment is reproduced with
  `snes-preview <rom> --state state.bin --frames 2 --shots 1 --out DIR`.

## Building
- Rust workspace in `rust/`. Mesen 2 is the submodule `external/mesen2`, changed only through
  `rust/crates/zax-snes/patches/`.
- CI's "Core boot check" runs clippy and the tests of the SNES crates and the renderer.
- App tests: `cargo test -p zax-app -- --test-threads=1` (only one Mesen may run at a time).
- `cargo fmt` only on `zax-snes` and `xsnes-preview` (the copied crates keep their format).
