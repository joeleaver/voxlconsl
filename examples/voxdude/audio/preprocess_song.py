#!/usr/bin/env python3
"""Convert the raw stems2midi output for voxdude into a cart-ready SMF.

The stems2midi tool emits one MIDI track per detected stem, each with
its own GM `program_change` to pick a General-MIDI instrument. For
voxdude we ignore the GM bank entirely and route every channel to one
of a handful of chiptune synth patches via cart-side `program_change`
calls in `init()`. So this script strips every `program_change` event
out of the source SMF and leaves everything else (timing, notes,
controllers, pitch bend, tempo, time-sig) untouched.

Run from anywhere; defaults point at the canonical source/destination:

    python3 examples/voxdude/audio/preprocess_song.py
"""

import os
import struct
import sys
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[3]
DEFAULT_SRC = REPO_ROOT.parent / "stems2midi" / "output" / "Voxel Synapse Stems.mid"
DEFAULT_DST = REPO_ROOT / "examples" / "voxdude" / "audio" / "songs" / "voxel-synapse.mid"


def read_vlq(data, j):
    v = 0
    while True:
        b = data[j]
        j += 1
        v = (v << 7) | (b & 0x7F)
        if not (b & 0x80):
            return v, j


def write_vlq(v):
    if v < 0:
        raise ValueError("VLQ must be non-negative")
    if v == 0:
        return b"\x00"
    out = []
    while v:
        out.append(v & 0x7F)
        v >>= 7
    out.reverse()
    buf = bytearray()
    for i, b in enumerate(out):
        if i < len(out) - 1:
            buf.append(b | 0x80)
        else:
            buf.append(b)
    return bytes(buf)


def strip_program_changes(track_bytes):
    """Walk one MTrk payload, copying every event verbatim except
    `program_change` (status 0xCx, 2 bytes total including status).

    Running-status is the trick: if we drop a status byte, the next
    same-status event that previously relied on running status loses
    its anchor. Easiest reliable handling: re-emit explicit status on
    every channel-voice event we keep (it costs a few bytes but is
    bullet-proof).
    """
    out_events = []  # list of (delta_ticks, raw_bytes)
    j = 0
    end = len(track_bytes)
    running_status = 0
    accumulated_delta = 0  # delta of dropped events folds into the next kept event

    while j < end:
        delta, j = read_vlq(track_bytes, j)
        accumulated_delta += delta

        # Peek status / running status.
        if track_bytes[j] & 0x80:
            status = track_bytes[j]
            j += 1
            if status < 0xF0:
                running_status = status
        else:
            status = running_status

        if status == 0xFF:
            # Meta event: 0xFF type vlq-length data...
            mtype = track_bytes[j]
            j += 1
            mlen, j = read_vlq(track_bytes, j)
            payload = track_bytes[j:j + mlen]
            j += mlen
            ev = bytes([0xFF, mtype]) + write_vlq(mlen) + payload
            out_events.append((accumulated_delta, ev))
            accumulated_delta = 0
            if mtype == 0x2F:
                break
        elif status == 0xF0 or status == 0xF7:
            # Sysex: status vlq-length data...
            mlen, j = read_vlq(track_bytes, j)
            payload = track_bytes[j:j + mlen]
            j += mlen
            ev = bytes([status]) + write_vlq(mlen) + payload
            out_events.append((accumulated_delta, ev))
            accumulated_delta = 0
        else:
            hi = status & 0xF0
            if hi == 0xC0 or hi == 0xD0:
                data1 = track_bytes[j]
                j += 1
                if hi == 0xC0:
                    # Drop program_change. Delta stays accumulated.
                    continue
                else:
                    ev = bytes([status, data1])
                    out_events.append((accumulated_delta, ev))
                    accumulated_delta = 0
            elif hi in (0x80, 0x90, 0xA0, 0xB0, 0xE0):
                data1 = track_bytes[j]
                data2 = track_bytes[j + 1]
                j += 2
                ev = bytes([status, data1, data2])
                out_events.append((accumulated_delta, ev))
                accumulated_delta = 0
            else:
                raise ValueError(f"unknown status byte 0x{status:02x} at offset {j}")

    body = bytearray()
    for delta, ev in out_events:
        body.extend(write_vlq(delta))
        body.extend(ev)
    return bytes(body)


def main():
    src = Path(sys.argv[1]) if len(sys.argv) > 1 else DEFAULT_SRC
    dst = Path(sys.argv[2]) if len(sys.argv) > 2 else DEFAULT_DST

    if not src.exists():
        print(f"source MIDI not found: {src}", file=sys.stderr)
        sys.exit(1)

    data = src.read_bytes()
    if data[:4] != b"MThd":
        print("not a MIDI file (no MThd header)", file=sys.stderr)
        sys.exit(1)

    hlen = struct.unpack(">I", data[4:8])[0]
    fmt = struct.unpack(">H", data[8:10])[0]
    ntrks = struct.unpack(">H", data[10:12])[0]
    div = struct.unpack(">H", data[12:14])[0]
    if fmt not in (0, 1):
        print(f"unsupported SMF format {fmt} (only 0 and 1 are handled)", file=sys.stderr)
        sys.exit(1)

    out = bytearray()
    out.extend(b"MThd")
    out.extend(struct.pack(">I", 6))
    out.extend(struct.pack(">HHH", fmt, ntrks, div))

    pgm_dropped = 0
    i = 14 + (hlen - 6)  # spec says header may be longer than 6 in future, jump past it
    for tidx in range(ntrks):
        if data[i:i + 4] != b"MTrk":
            print(f"track {tidx}: missing MTrk magic at offset {i}", file=sys.stderr)
            sys.exit(1)
        tlen = struct.unpack(">I", data[i + 4:i + 8])[0]
        body = data[i + 8:i + 8 + tlen]
        before = body.count(b"") + sum(1 for b in body if 0xC0 <= b < 0xD0)
        stripped = strip_program_changes(body)
        out.extend(b"MTrk")
        out.extend(struct.pack(">I", len(stripped)))
        out.extend(stripped)
        i += 8 + tlen
        # Approximate program_change drop count for reporting; running
        # status means raw byte scan over-counts so just report bytes diff.
        pgm_dropped += (len(body) - len(stripped))

    dst.parent.mkdir(parents=True, exist_ok=True)
    dst.write_bytes(out)
    print(f"wrote {dst}  ({len(out)} bytes, was {len(data)}; saved {len(data) - len(out)} bytes from program_change strip)")


if __name__ == "__main__":
    main()
