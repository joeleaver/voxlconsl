#!/usr/bin/env python3
"""Convert a figlet .flf font into voxlconsl's .vfnt format.

The .flf format stores each glyph as `height` text rows. To get good
vertical resolution out of a 9-row figlet font we decode unicode
half-block characters:

    ' '  (space)           → 0/0  (top empty,    bottom empty)
    '▀'  U+2580 UPPER HALF → 1/0  (top filled,   bottom empty)
    '▄'  U+2584 LOWER HALF → 0/1  (top empty,    bottom filled)
    '█'  U+2588 FULL BLOCK → 1/1  (top filled,   bottom filled)

Anything else in the visible columns is treated as filled. The flf
hardblank ('$' or whatever the header declares) is treated as space.

Usage:
    flf_to_vfnt.py <input.flf> <output.vfnt> [--first 32] [--last 126]

The output is a fixed-width .vfnt covering codepoints first..=last
(inclusive). The cell width is taken from the .flf max-length header
field (or the widest decoded glyph, whichever fits). The cell height is
2 * (flf height) trimmed of always-blank bottom rows.

This is intentionally a one-shot generator. The repo commits the
resulting .vfnt blob and the SDK includes it via `include_bytes!`.
"""

from __future__ import annotations

import argparse
import struct
import sys
from pathlib import Path


HALF_BLOCK_DECODE = {
    " ":     (0, 0),
    "▀": (1, 0),  # ▀
    "▄": (0, 1),  # ▄
    "█": (1, 1),  # █
}


def parse_flf(path: Path):
    raw = path.read_bytes().decode("utf-8")
    lines = raw.split("\n")
    if not lines or not lines[0].startswith("flf2a"):
        raise SystemExit(f"{path}: not an flf2a font")

    header = lines[0].split()
    # flf2a<hardblank> height baseline maxlen oldlayout commentlines ...
    hardblank = header[0][5]
    height = int(header[1])
    max_length = int(header[3])
    comment_lines = int(header[5])

    body_start = 1 + comment_lines
    body = lines[body_start:]

    # Glyphs are sequential, codepoint 32, 33, 34, ... up to (and including)
    # 126. After that figlet adds Ä Ö Ü ä ö ü ß which we ignore here.
    glyphs = {}
    cp = 32
    i = 0
    while i + height <= len(body):
        if cp > 126:
            break
        rows_text = body[i : i + height]
        i += height
        # Strip trailing endmark on each row. Figlet uses one endmark
        # ('@' typically) on every row except the last, which has two.
        stripped = []
        for ridx, row in enumerate(rows_text):
            r = row
            # Pull the endmark character off the right edge.
            if r.endswith("@@"):
                r = r[:-2]
            elif r.endswith("@"):
                r = r[:-1]
            # Pad / trim to max_length so all rows are the same width.
            if len(r) < max_length:
                r = r + " " * (max_length - len(r))
            else:
                r = r[:max_length]
            # Replace hardblank with space.
            r = r.replace(hardblank, " ")
            stripped.append(r)
        glyphs[cp] = stripped
        cp += 1

    return height, max_length, glyphs


def uses_half_blocks(glyphs_text):
    """Return True if any glyph row contains a unicode half-block char.

    Half-block fonts (e.g. Delta Corps Priest 1) pack 2 pixel rows per text row;
    plain fonts (e.g. ANSI Regular, drawn entirely with `#`/space) are 1:1.
    """
    for rows in glyphs_text.values():
        for r in rows:
            if any(ch in HALF_BLOCK_DECODE and ch != " " for ch in r):
                return True
    return False


def decode_glyph(rows_text, max_length, double):
    """Decode one glyph's text rows into a pixel grid.

    `double = True` interprets unicode half-blocks (`▀▄█`) and emits 2 pixel
    rows per text row. `double = False` treats every non-space char as a
    filled pixel and emits 1 pixel row per text row.
    """
    pixel_rows = []
    for r in rows_text:
        if double:
            top = []
            bot = []
            for ch in r:
                t, b = HALF_BLOCK_DECODE.get(
                    ch,
                    (1 if ch.strip() else 0, 1 if ch.strip() else 0),
                )
                top.append(t)
                bot.append(b)
            if len(top) < max_length:
                top += [0] * (max_length - len(top))
                bot += [0] * (max_length - len(bot))
            else:
                top = top[:max_length]
                bot = bot[:max_length]
            pixel_rows.append(top)
            pixel_rows.append(bot)
        else:
            row = []
            for ch in r:
                row.append(1 if (ch != " " and ch.strip()) else 0)
            if len(row) < max_length:
                row += [0] * (max_length - len(row))
            else:
                row = row[:max_length]
            pixel_rows.append(row)
    return pixel_rows


def trim_blank_rows(grid):
    """Trim trailing blank pixel rows shared across all glyphs.

    Operates on a dict of {cp: 2D-bitmap}. Returns the trimmed grids and
    the new height.
    """
    if not grid:
        return grid, 0
    h = max(len(b) for b in grid.values())
    # Find the lowest row index that is non-blank in any glyph.
    last_nonblank = -1
    for cp, bm in grid.items():
        for ridx, row in enumerate(bm):
            if any(row):
                if ridx > last_nonblank:
                    last_nonblank = ridx
    new_h = last_nonblank + 1
    trimmed = {cp: bm[:new_h] for cp, bm in grid.items()}
    return trimmed, new_h


def trim_blank_cols(grid, width):
    """Trim shared trailing blank columns. Returns trimmed grids + new width."""
    if not grid:
        return grid, 0
    last_nonblank = -1
    for cp, bm in grid.items():
        for row in bm:
            for cidx, v in enumerate(row):
                if v and cidx > last_nonblank:
                    last_nonblank = cidx
    new_w = last_nonblank + 1
    trimmed = {cp: [row[:new_w] for row in bm] for cp, bm in grid.items()}
    return trimmed, new_w


def encode_vfnt(grids, cell_w, cell_h, first_cp, last_cp):
    """Pack glyphs into a .vfnt byte buffer.

    Glyph order: ascending codepoint, fixed-width.
    Bitmap layout: cell_w * cell_h bits per glyph, MSB-first, row-major,
    rounded up to whole bytes per glyph.
    """
    cps = [cp for cp in range(first_cp, last_cp + 1) if cp in grids]
    glyph_count = len(cps)
    bytes_per_glyph = (cell_w * cell_h + 7) // 8

    header = bytearray(16)
    header[0:4] = b"VFN1"
    header[4] = 1            # version
    header[5] = cell_w
    header[6] = cell_h
    header[7] = 0            # flags
    struct.pack_into("<H", header, 8, glyph_count)
    # bytes 10..16 reserved zero

    index = bytearray()
    for i, cp in enumerate(cps):
        index += struct.pack("<II", cp, i * bytes_per_glyph)

    bitmaps = bytearray()
    for cp in cps:
        bm = grids[cp]
        bits = []
        for row in bm:
            for v in row:
                bits.append(1 if v else 0)
        # Pad to whole byte
        while len(bits) % 8 != 0:
            bits.append(0)
        for byte_start in range(0, len(bits), 8):
            byte = 0
            for j in range(8):
                if bits[byte_start + j]:
                    byte |= 1 << (7 - j)
            bitmaps.append(byte)

    return bytes(header) + bytes(index) + bytes(bitmaps)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("input", type=Path)
    ap.add_argument("output", type=Path)
    ap.add_argument("--first", type=int, default=32)
    ap.add_argument("--last", type=int, default=126)
    ap.add_argument("--no-trim-rows", action="store_true",
                    help="Keep all decoded pixel rows; default trims shared trailing blanks.")
    ap.add_argument("--no-trim-cols", action="store_true",
                    help="Keep full max_length; default trims shared trailing blank columns.")
    args = ap.parse_args()

    flf_height, flf_maxlen, glyphs_text = parse_flf(args.input)

    double = uses_half_blocks(glyphs_text)
    grids = {cp: decode_glyph(rows, flf_maxlen, double)
             for cp, rows in glyphs_text.items()}
    if not grids:
        raise SystemExit("no glyphs decoded")

    if not args.no_trim_rows:
        grids, cell_h = trim_blank_rows(grids)
    else:
        cell_h = 2 * flf_height

    if not args.no_trim_cols:
        grids, cell_w = trim_blank_cols(grids, flf_maxlen)
    else:
        cell_w = flf_maxlen

    blob = encode_vfnt(grids, cell_w, cell_h, args.first, args.last)
    args.output.write_bytes(blob)

    print(f"wrote {args.output} ({len(blob)} B), cell {cell_w}x{cell_h}, "
          f"glyphs {len([cp for cp in range(args.first, args.last+1) if cp in grids])}",
          file=sys.stderr)


if __name__ == "__main__":
    main()
