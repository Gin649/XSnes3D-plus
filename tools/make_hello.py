#!/usr/bin/env python3
"""Builds tests/roms/hello.sfc, the tiny SNES test ROM of XSnes3D+.

The ROM is ours (GPL-3.0 like the rest of the repo). It turns the screen dark blue and draws
a yellow rectangle on background 1: tiles 4 to 11 across and 4 to 7 down, so pixels 32 to 95
across and 32 to 63 down. Then it loops forever. The boot test checks those colours.

Usage: python3 tools/make_hello.py [output path]
"""

import struct
import sys
from pathlib import Path

ROM_SIZE = 0x8000  # 32 KiB, one LoROM bank at $00:8000
CODE_AT = 0x0000  # $00:8000
CHR_AT = 0x1000  # tile data, $00:9000
MAP_AT = 0x2000  # tile map, $00:A000

BACKDROP = 20 << 10  # blue 20 of 31, as BGR555
YELLOW = 0x03FF  # red 31, green 31


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

    # Colours: entry 0 is the backdrop, entry 1 the tile colour.
    stz(0x2121)
    for colour in (BACKDROP, YELLOW):
        lda(colour & 0xFF)
        sta(0x2122)
        lda(colour >> 8)
        sta(0x2122)

    stz(0x2105)  # mode 0
    lda(0x04)
    sta(0x2107)  # BG1 tile map at VRAM word $0400
    stz(0x210B)  # BG1 tiles at VRAM word $0000
    stz(0x210D)
    stz(0x210D)  # BG1 horizontal scroll 0
    lda(0xFF)
    sta(0x210E)
    sta(0x210E)  # BG1 vertical scroll -1, so the map's line 0 is the first line shown
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

    dma_to_vram(0x0000, CHR_AT, 32)  # tiles 0 and 1
    dma_to_vram(0x0400, MAP_AT, 2048)  # the 32 by 32 tile map

    lda(0x01)
    sta(0x212C)  # BG1 on the main screen
    lda(0x0F)
    sta(0x2100)  # screen on, full brightness
    emit(0x80, 0xFE)  # bra to itself
    return out


def tiles():
    # 2 bits per pixel: tile 0 is all colour 0, tile 1 all colour 1 (low plane set).
    blank = bytes(16)
    solid = bytes([0xFF, 0x00] * 8)
    return blank + solid


def tile_map():
    words = []
    for row in range(32):
        for column in range(32):
            words.append(1 if 4 <= column <= 11 and 4 <= row <= 7 else 0)
    return struct.pack("<1024H", *words)


def build():
    rom = bytearray(ROM_SIZE)
    program = code()
    rom[CODE_AT : CODE_AT + len(program)] = program
    rom[CHR_AT : CHR_AT + 32] = tiles()
    rom[MAP_AT : MAP_AT + 2048] = tile_map()

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
