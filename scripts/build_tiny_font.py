#!/usr/bin/env python3
"""Build `crates/sdk/src/fonts/tiny.vfnt` from inline glyph specs.

FONT_TINY is a 4×6 voxel font — the smallest legible size for the
256×144 framebuffer when one voxel projects to roughly one screen
pixel (the regime where text rendered via cart actors lives). Cell
is 4 wide × 6 tall; glyphs sit in the top 5 rows with the bottom
row reserved for descenders (q g j p y) and the comma / semicolon
hook.

Each glyph below is given as 6 rows of 4 chars, where `#` is on
and `.` is off. Sorted by codepoint, packed into VFN1 v1 format
(SPEC.md §12.7).
"""
from pathlib import Path
import struct


# --- glyph specs ----------------------------------------------------
#
# Indented for readability. Whitespace before the glyph block is
# stripped at parse time. Each block must be exactly 6 lines of 4
# chars (.#) — the loader asserts this.
#
# Codepoint ordering matters: must be ascending. Within a glyph,
# row 0 is the *top* of the letter.

GLYPHS: dict[int, str] = {
    0x20: """
        ....
        ....
        ....
        ....
        ....
        ....
    """,
    # ! " # $ % & '
    0x21: """
        .#..
        .#..
        .#..
        ....
        .#..
        ....
    """,
    0x22: """
        #.#.
        #.#.
        ....
        ....
        ....
        ....
    """,
    0x23: """
        .#.#
        ####
        .#.#
        ####
        .#.#
        ....
    """,
    0x24: """
        .###
        #.#.
        .##.
        .#.#
        ###.
        ....
    """,
    0x25: """
        #..#
        ...#
        ..#.
        .#..
        #..#
        ....
    """,
    0x26: """
        .#..
        #.#.
        .#..
        #.#.
        .##.
        ....
    """,
    0x27: """
        .#..
        .#..
        ....
        ....
        ....
        ....
    """,
    0x28: """
        ..#.
        .#..
        .#..
        .#..
        ..#.
        ....
    """,
    0x29: """
        .#..
        ..#.
        ..#.
        ..#.
        .#..
        ....
    """,
    0x2A: """
        ....
        #.#.
        .#..
        #.#.
        ....
        ....
    """,
    0x2B: """
        ....
        .#..
        ###.
        .#..
        ....
        ....
    """,
    0x2C: """
        ....
        ....
        ....
        ....
        .#..
        #...
    """,
    0x2D: """
        ....
        ....
        ###.
        ....
        ....
        ....
    """,
    0x2E: """
        ....
        ....
        ....
        ....
        .#..
        ....
    """,
    0x2F: """
        ...#
        ..#.
        ..#.
        .#..
        .#..
        ....
    """,
    # 0-9
    0x30: """
        .##.
        #..#
        #..#
        #..#
        .##.
        ....
    """,
    0x31: """
        .#..
        ##..
        .#..
        .#..
        ###.
        ....
    """,
    0x32: """
        ###.
        ..#.
        .#..
        #...
        ####
        ....
    """,
    0x33: """
        ###.
        ..#.
        .##.
        ..#.
        ###.
        ....
    """,
    0x34: """
        #.#.
        #.#.
        ####
        ..#.
        ..#.
        ....
    """,
    0x35: """
        ####
        #...
        ###.
        ...#
        ###.
        ....
    """,
    0x36: """
        .##.
        #...
        ###.
        #..#
        .##.
        ....
    """,
    0x37: """
        ####
        ...#
        ..#.
        .#..
        .#..
        ....
    """,
    0x38: """
        .##.
        #..#
        .##.
        #..#
        .##.
        ....
    """,
    0x39: """
        .##.
        #..#
        .###
        ...#
        .##.
        ....
    """,
    0x3A: """
        ....
        .#..
        ....
        .#..
        ....
        ....
    """,
    0x3B: """
        ....
        .#..
        ....
        ....
        .#..
        #...
    """,
    0x3C: """
        ..#.
        .#..
        #...
        .#..
        ..#.
        ....
    """,
    0x3D: """
        ....
        ###.
        ....
        ###.
        ....
        ....
    """,
    0x3E: """
        #...
        .#..
        ..#.
        .#..
        #...
        ....
    """,
    0x3F: """
        ###.
        ..#.
        .##.
        ....
        .#..
        ....
    """,
    0x40: """
        .##.
        #..#
        #.##
        #...
        .##.
        ....
    """,
    # A-Z
    0x41: """
        .##.
        #..#
        ####
        #..#
        #..#
        ....
    """,
    0x42: """
        ###.
        #..#
        ###.
        #..#
        ###.
        ....
    """,
    0x43: """
        .###
        #...
        #...
        #...
        .###
        ....
    """,
    0x44: """
        ###.
        #..#
        #..#
        #..#
        ###.
        ....
    """,
    0x45: """
        ####
        #...
        ###.
        #...
        ####
        ....
    """,
    0x46: """
        ####
        #...
        ###.
        #...
        #...
        ....
    """,
    0x47: """
        .###
        #...
        #.##
        #..#
        .###
        ....
    """,
    0x48: """
        #..#
        #..#
        ####
        #..#
        #..#
        ....
    """,
    0x49: """
        ###.
        .#..
        .#..
        .#..
        ###.
        ....
    """,
    0x4A: """
        ..##
        ...#
        ...#
        #..#
        .##.
        ....
    """,
    0x4B: """
        #..#
        #.#.
        ##..
        #.#.
        #..#
        ....
    """,
    0x4C: """
        #...
        #...
        #...
        #...
        ####
        ....
    """,
    0x4D: """
        #..#
        ####
        ####
        #..#
        #..#
        ....
    """,
    0x4E: """
        #..#
        ##.#
        ####
        #.##
        #..#
        ....
    """,
    0x4F: """
        .##.
        #..#
        #..#
        #..#
        .##.
        ....
    """,
    0x50: """
        ###.
        #..#
        ###.
        #...
        #...
        ....
    """,
    0x51: """
        .##.
        #..#
        #..#
        #.#.
        .#.#
        ....
    """,
    0x52: """
        ###.
        #..#
        ###.
        #.#.
        #..#
        ....
    """,
    0x53: """
        .###
        #...
        .##.
        ...#
        ###.
        ....
    """,
    0x54: """
        ####
        .#..
        .#..
        .#..
        .#..
        ....
    """,
    0x55: """
        #..#
        #..#
        #..#
        #..#
        .##.
        ....
    """,
    0x56: """
        #..#
        #..#
        #..#
        .##.
        .##.
        ....
    """,
    0x57: """
        #..#
        #..#
        ####
        ####
        #..#
        ....
    """,
    0x58: """
        #..#
        .##.
        .##.
        .##.
        #..#
        ....
    """,
    0x59: """
        #..#
        #..#
        .##.
        .#..
        .#..
        ....
    """,
    0x5A: """
        ####
        ..#.
        .#..
        #...
        ####
        ....
    """,
    0x5B: """
        .##.
        .#..
        .#..
        .#..
        .##.
        ....
    """,
    0x5C: """
        .#..
        .#..
        ..#.
        ..#.
        ...#
        ....
    """,
    0x5D: """
        .##.
        ..#.
        ..#.
        ..#.
        .##.
        ....
    """,
    0x5E: """
        .#..
        #.#.
        ....
        ....
        ....
        ....
    """,
    0x5F: """
        ....
        ....
        ....
        ....
        ....
        ####
    """,
}


# --- encoder --------------------------------------------------------

CELL_W = 4
CELL_H = 6
BYTES_PER_GLYPH = (CELL_W * CELL_H + 7) // 8     # 24 bits → 3 bytes


def parse_glyph(spec: str) -> bytes:
    """Parse a 6-row × 4-col `#./` block into packed bytes.

    Bits are MSB-first, row-major, matching `Font::glyph_bit`. The
    glyph's bit i is `byte = bitmap[i // 8], mask = 1 << (7 - i % 8)`.
    """
    rows = [line.strip() for line in spec.strip("\n").splitlines() if line.strip() != ""]
    if len(rows) != CELL_H:
        raise ValueError(f"expected {CELL_H} rows, got {len(rows)}: {rows!r}")
    bits = []
    for row in rows:
        if len(row) != CELL_W:
            raise ValueError(f"row width {len(row)} != {CELL_W}: {row!r}")
        for ch in row:
            if ch == "#":
                bits.append(1)
            elif ch == ".":
                bits.append(0)
            else:
                raise ValueError(f"glyph row {row!r} has non-#/. char {ch!r}")
    out = bytearray(BYTES_PER_GLYPH)
    for i, b in enumerate(bits):
        if b:
            out[i // 8] |= 1 << (7 - (i % 8))
    return bytes(out)


def build_vfnt(glyphs: dict[int, str]) -> bytes:
    sorted_cps = sorted(glyphs.keys())
    n = len(sorted_cps)
    if n > 0xFFFF:
        raise ValueError("too many glyphs")

    # Header (16 bytes)
    header = bytearray(16)
    header[0:4] = b"VFN1"
    header[4]   = 1            # version
    header[5]   = CELL_W
    header[6]   = CELL_H
    header[7]   = 0            # flags
    header[8:10] = struct.pack("<H", n)
    # bytes 10..16 reserved (already zero)

    # Index
    index = bytearray()
    for i, cp in enumerate(sorted_cps):
        offset = i * BYTES_PER_GLYPH
        index += struct.pack("<II", cp, offset)

    # Bitmap
    bitmap = bytearray()
    for cp in sorted_cps:
        bitmap += parse_glyph(glyphs[cp])

    return bytes(header) + bytes(index) + bytes(bitmap)


def main() -> None:
    out_path = Path(__file__).resolve().parent.parent / "crates" / "sdk" / "src" / "fonts" / "tiny.vfnt"
    data = build_vfnt(GLYPHS)
    out_path.write_bytes(data)
    print(f"wrote {len(data)} bytes -> {out_path}")
    print(f"  glyphs: {len(GLYPHS)}  cell: {CELL_W}×{CELL_H}  bytes/glyph: {BYTES_PER_GLYPH}")


if __name__ == "__main__":
    main()
