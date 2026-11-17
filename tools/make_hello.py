#!/usr/bin/env python3
"""Builds tests/roms/hello.sfc, the tiny SNES test ROM of XSnes3D+.

The ROM is ours (GPL-3.0 like the rest of the repo). In background mode 1 it draws, on a dark
blue backdrop:
- a yellow rectangle on background 1 (16 colours): pixels 32 to 95 across, 32 to 63 down;
- a green band on background 3 (4 colours), behind it: all across, 48 to 79 down;
- a red 8 by 8 sprite in front of both, at 120 across, 52 down.
Then it loops forever. The boot and tile tests check those colours and the recorded tiles.

Usage: python3 tools/make_hello.py [output path]
"""

import struct
import sys
from pathlib import Path

ROM_SIZE = 0x8000  # 32 KiB, one LoROM bank at $00:8000
CODE_AT = 0x0000  # $00:8000
BG1_CHR_AT = 0x1000  # background 1 tiles (4 bits per pixel), $00:9000
BG3_CHR_AT = 0x1100  # background 3 tiles (2 bits per pixel)
OBJ_CHR_AT = 0x1200  # sprite tiles (4 bits per pixel)
OAM_AT = 0x1300  # sprite table, 544 bytes
BG1_MAP_AT = 0x2000  # background 1 tile map, $00:A000
BG3_MAP_AT = 0x2800  # background 3 tile map

SPRITE_X = 120
SPRITE_Y = 52  # the first picture line the sprite covers

BACKDROP = 20 << 10  # blue 20 of 31, as BGR555
YELLOW = 0x03FF  # red 31, green 31
GREEN = 0x03E0  # green 31
RED = 0x001F  # red 31


def bank0(offset):
    return 0x8000 + offset


def code():
    out = bytearray()

    def emit(*values):
        out.extend(values)

    def lda(value):
        emit(0xA9, value)

    def sta(address):
        emit(0x8D, address & 0xFF, address >> 8)

    def stz(address):
        emit(0x9C, address & 0xFF, address >> 8)

    def ldx(value):
        emit(0xA2, value & 0xFF, value >> 8)

    def stx(address):
        emit(0x8E, address & 0xFF, address >> 8)

    emit(0x78)  # sei
    emit(0x18, 0xFB)  # clc, xce: native mode
    emit(0xC2, 0x10)  # rep #$10: 16-bit X and Y
    emit(0xE2, 0x20)  # sep #$20: 8-bit A
    ldx(0x1FFF)
    emit(0x9A)  # txs
    lda(0x80)
    sta(0x2100)  # screen off while we set things up
    stz(0x4200)  # no interrupts

    # Colours: 0 backdrop, 1 yellow (BG1 palette 0), 5 green (BG3 palette 1),
    # 129 red (sprite palette 0).
    def colour_at(index, colour):
        lda(index)
        sta(0x2121)
        lda(colour & 0xFF)
        sta(0x2122)
        lda(colour >> 8)
        sta(0x2122)

    colour_at(0, BACKDROP)
    colour_at(1, YELLOW)
    colour_at(5, GREEN)
    colour_at(129, RED)

    lda(0x01)
    sta(0x2105)  # mode 1
    lda(0x04)
    sta(0x2107)  # BG1 tile map at VRAM word $0400
    lda(0x08)
    sta(0x2109)  # BG3 tile map at VRAM word $0800
    stz(0x210B)  # BG1 tiles at VRAM word $0000
    lda(0x01)
    sta(0x210C)  # BG3 tiles at VRAM word $1000
    lda(0x01)
    sta(0x2101)  # sprites 8 by 8, tiles at VRAM word $2000
    for scroll in (0x210D, 0x2111):
        stz(scroll)
        stz(scroll)  # horizontal scroll 0
    for scroll in (0x210E, 0x2112):
        lda(0xFF)
        sta(scroll)
        sta(scroll)  # vertical scroll -1, so the map's line 0 is the first line shown
    lda(0x80)
    sta(0x2115)  # VRAM address steps after each high byte

    def dma_to_vram(vram_word, rom_offset, length):
        ldx(vram_word)
        stx(0x2116)
        lda(0x01)
        sta(0x4300)  # two registers, $2118 then $2119
        lda(0x18)
        sta(0x4301)
        ldx(bank0(rom_offset))
        stx(0x4302)
        stz(0x4304)  # bank 0
        ldx(length)
        stx(0x4305)
        lda(0x01)
        sta(0x420B)  # start channel 0

    dma_to_vram(0x0000, BG1_CHR_AT, 64)  # BG1 tiles 0 and 1
    dma_to_vram(0x1000, BG3_CHR_AT, 32)  # BG3 tiles 0 and 1
    dma_to_vram(0x2000, OBJ_CHR_AT, 64)  # sprite tiles 0 and 1
    dma_to_vram(0x0400, BG1_MAP_AT, 2048)
    dma_to_vram(0x0800, BG3_MAP_AT, 2048)

    # The sprite table, through $2104
    stz(0x2102)
    stz(0x2103)
    stz(0x4300)  # one register
    lda(0x04)
    sta(0x4301)
    ldx(bank0(OAM_AT))
    stx(0x4302)
    stz(0x4304)
    ldx(544)
    stx(0x4305)
    lda(0x01)
    sta(0x420B)

    lda(0x15)
    sta(0x212C)  # BG1, BG3 and sprites on the main screen
    lda(0x0F)
    sta(0x2100)  # screen on, full brightness
    emit(0x80, 0xFE)  # bra to itself
    return out


def tiles_2bpp():
    # Tile 0 is all colour 0, tile 1 all colour 1 (plane 0 set).
    return bytes(16) + bytes([0xFF, 0x00] * 8)


def tiles_4bpp():
    # The same at 4 bits per pixel: planes 0-1 then planes 2-3, 8 rows each.
    return bytes(32) + bytes([0xFF, 0x00] * 8) + bytes(16)


def tile_map(inside, entry):
    words = []
    for row in range(32):
        for column in range(32):
            words.append(entry if inside(column, row) else 0)
    return struct.pack("<1024H", *words)


def sprite_table():
    table = bytearray()
    # Sprite 0: tile 1, palette 0, priority 3 (in front of everything).
    table += bytes([SPRITE_X, SPRITE_Y, 0x01, 0x30])
    for _ in range(127):
        table += bytes([0, 240, 0, 0])  # below the picture
    table += bytes(32)  # high table: small sprites, X below 256
    return bytes(table)


def build():
    rom = bytearray(ROM_SIZE)
    program = code()
    rom[CODE_AT : CODE_AT + len(program)] = program
    rom[BG1_CHR_AT : BG1_CHR_AT + 64] = tiles_4bpp()
    rom[BG3_CHR_AT : BG3_CHR_AT + 32] = tiles_2bpp()
    rom[OBJ_CHR_AT : OBJ_CHR_AT + 64] = tiles_4bpp()
    rom[OAM_AT : OAM_AT + 544] = sprite_table()
    # BG1: tile 1, palette 0. BG3: tile 1, palette 1 (colours 4 to 7).
    rom[BG1_MAP_AT : BG1_MAP_AT + 2048] = tile_map(lambda c, r: 4 <= c <= 11 and 4 <= r <= 7, 0x0001)
    rom[BG3_MAP_AT : BG3_MAP_AT + 2048] = tile_map(lambda c, r: 6 <= r <= 9, 0x0401)

    header = 0x7FC0
    rom[header : header + 21] = b"XSNES3D+ HELLO".ljust(21, b" ")
    rom[0x7FD5] = 0x20  # LoROM, slow
    rom[0x7FD6] = 0x00  # ROM only
    rom[0x7FD7] = 0x05  # 32 KiB
    rom[0x7FD8] = 0x00  # no save RAM
    rom[0x7FD9] = 0x01  # North America
    rom[0x7FDA] = 0x00
    rom[0x7FDB] = 0x00  # version 0

    reset = bank0(CODE_AT)
    for vector in range(0x7FE4, 0x8000, 2):
        struct.pack_into("<H", rom, vector, reset)

    # Checksum and its complement: the complement goes in first, so the sum counts both.
    struct.pack_into("<HH", rom, 0x7FDC, 0xFFFF, 0x0000)
    checksum = sum(rom) & 0xFFFF
    struct.pack_into("<HH", rom, 0x7FDC, checksum ^ 0xFFFF, checksum)
    return bytes(rom)


def main():
    root = Path(__file__).resolve().parent.parent
    target = Path(sys.argv[1]) if len(sys.argv) > 1 else root / "tests" / "roms" / "hello.sfc"
    target.parent.mkdir(parents=True, exist_ok=True)
    target.write_bytes(build())
    print(f"wrote {target}")


if __name__ == "__main__":
    main()
