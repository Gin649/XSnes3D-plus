# Third-party code

## Mesen 2

XSnes3D+ uses the SNES emulation core of Mesen 2, by Sour, as the git submodule
`external/mesen2` (pinned to release 2.1.1).

- Source: https://github.com/SourMesen/Mesen2
- Licence: GNU General Public License v3.0 (see `external/mesen2/LICENSE`)

Mesen 2 itself includes code under other licences (7-Zip's LZMA SDK, Lua, miniz, spng and
others); their notices are in the Mesen 2 source tree.

XSnes3D+ changes Mesen 2 only through the patches in `rust/crates/zax-snes/patches/`, which
are applied to a copy of its source at build time. The submodule is never modified.

## Xnes3D+

The shared 3D engine, renderer and test harness come from Xnes3D+ (GPL-3.0), as the git
submodule `external/xnes3d-plus`. Xnes3D+ is a fork of Zaxnes by nixxou.

- Source: https://github.com/Gin649/Xnes3D-plus
