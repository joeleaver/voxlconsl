#!/usr/bin/env python3
"""Regenerate big-world's bundled audio assets.

Produces two files committed alongside this script:

  samples/beep.wav  — 4096-frame mono 22.05 kHz triangle+saw waveform
                      with a forward smpl-chunk loop region (64, 4032).
                      Same shape as the procedural sample big-world used
                      to synthesise inside init() before the audio
                      section landed.

  songs/groove.mid  — one-bar (PPQ 96, 120 BPM) drums + lead + bell +
                      sampler-pad groove. Same event list as the
                      hand-built SMF buffer big-world used to push
                      through `music_load`.

Run from this directory:

    python3 build_assets.py

Pure stdlib — no numpy / mido / etc. so the script is portable.
"""

from __future__ import annotations

import struct
import wave
from pathlib import Path

HERE = Path(__file__).resolve().parent
SAMPLES = HERE / "samples"
SONGS = HERE / "songs"
SAMPLES.mkdir(parents=True, exist_ok=True)
SONGS.mkdir(parents=True, exist_ok=True)


# ── beep.wav ────────────────────────────────────────────────────────────────
# Replicates the procedural sample that big-world's lib.rs synthesised
# at init time (triangle + saw blend, periodic, 22.05 kHz, with edge
# tapers). Wraps a `smpl` chunk so the bundler picks up loop points
# without the cart having to declare them in patches.toml.

SAMPLE_RATE = 22_050
SAMPLE_LEN = 4096
LOOP_START = 64
LOOP_END = SAMPLE_LEN - 64  # exclusive (engine convention)
FREQ_AT_ROOT = 261.63  # C4


def synth_beep_pcm() -> bytes:
    period_samples = max(1, int(SAMPLE_RATE / FREQ_AT_ROOT))
    out = bytearray(SAMPLE_LEN)
    for i in range(SAMPLE_LEN):
        phase = (i % period_samples) / period_samples
        tri = 1.0 - 4.0 * abs(phase - 0.5)
        saw = 2.0 * phase - 1.0
        s = 0.75 * tri + 0.25 * saw
        if i < 64:
            taper = i / 64.0
        elif i > SAMPLE_LEN - 64:
            taper = (SAMPLE_LEN - i) / 64.0
        else:
            taper = 1.0
        sample = s * 100.0 * taper + 128.0
        out[i] = max(0, min(255, int(sample)))
    return bytes(out)


def write_beep_wav(path: Path) -> None:
    pcm = synth_beep_pcm()
    # `wave` doesn't write `smpl` chunks, so we build the file by hand.
    fmt_chunk = struct.pack(
        "<HHIIHH",
        1,          # PCM
        1,          # mono
        SAMPLE_RATE,
        SAMPLE_RATE,  # byte rate
        1,            # block align
        8,            # bits
    )
    data_chunk = pcm
    # smpl header (36 bytes):
    #   manufacturer / product / sample_period / midi_unity_note /
    #   midi_pitch_fraction / smpte_format / smpte_offset /
    #   num_sample_loops / sampler_data
    smpl_header = struct.pack(
        "<IIIIIIIII",
        0,   # manufacturer
        0,   # product
        0,   # sample_period
        60,  # midi_unity_note (C4)
        0,   # midi_pitch_fraction
        0,   # smpte_format
        0,   # smpte_offset
        1,   # num_sample_loops
        0,   # sampler_data
    )
    # loop record (24 bytes):
    #   cue_point_id / type (0 = forward) / start / end (inclusive) /
    #   fraction / play_count (0 = infinite)
    smpl_loop = struct.pack(
        "<IIIIII",
        0,
        0,
        LOOP_START,
        LOOP_END - 1,
        0,
        0,
    )
    smpl_chunk = smpl_header + smpl_loop

    body = b"WAVE"
    body += b"fmt " + struct.pack("<I", len(fmt_chunk)) + fmt_chunk
    body += b"data" + struct.pack("<I", len(data_chunk)) + data_chunk
    body += b"smpl" + struct.pack("<I", len(smpl_chunk)) + smpl_chunk

    out = b"RIFF" + struct.pack("<I", len(body)) + body
    path.write_bytes(out)


# ── groove.mid ──────────────────────────────────────────────────────────────
# Drop-in equivalent of big-world's hand-assembled SMF: format 0, PPQ 96,
# one bar (384 ticks) at the default 120 BPM tempo, with kick + hat +
# snare on channel 9, saw lead on channel 0, FM2OP bell strikes on
# channel 2, and sampler pad sustains on channel 3.

PPQ = 96

DEMO_EVENTS: list[tuple[int, int, int, int]] = [
    # tick 0: kick + hat + lead A3 + bell A5 + sampler pad A3
    (0,   0x99, 36, 110),
    (0,   0x99, 42, 70),
    (0,   0x90, 57, 100),
    (0,   0x92, 81, 90),
    (0,   0x93, 57, 80),
    (40,  0x80, 57, 0),
    # tick 48: hat + lead C4
    (48,  0x99, 42, 70),
    (48,  0x90, 60, 100),
    (88,  0x80, 60, 0),
    # tick 96: snare + hat + lead D4
    (96,  0x99, 38, 100),
    (96,  0x99, 42, 70),
    (96,  0x90, 62, 100),
    (136, 0x80, 62, 0),
    # tick 144: hat + lead E4
    (144, 0x99, 42, 70),
    (144, 0x90, 64, 100),
    (184, 0x80, 64, 0),
    # tick 188: bell + pad off
    (188, 0x82, 81, 0),
    (188, 0x83, 57, 0),
    # tick 192 (beat 3): kick + hat + lead G4 + bell E5 + pad E3
    (192, 0x99, 36, 110),
    (192, 0x99, 42, 70),
    (192, 0x90, 67, 100),
    (192, 0x92, 76, 90),
    (192, 0x93, 52, 80),
    (232, 0x80, 67, 0),
    # tick 240: hat + lead E4
    (240, 0x99, 42, 70),
    (240, 0x90, 64, 100),
    (280, 0x80, 64, 0),
    # tick 288: snare + hat + lead D4
    (288, 0x99, 38, 100),
    (288, 0x99, 42, 70),
    (288, 0x90, 62, 100),
    (328, 0x80, 62, 0),
    # tick 336: hat + lead C4
    (336, 0x99, 42, 70),
    (336, 0x90, 60, 100),
    (376, 0x80, 60, 0),
    # tick 380: bell + pad off just before bar wrap
    (380, 0x82, 76, 0),
    (380, 0x83, 52, 0),
]

BAR_LENGTH_TICKS = 384


def write_vlq(v: int) -> bytes:
    out = [v & 0x7F]
    v >>= 7
    while v > 0:
        out.append((v & 0x7F) | 0x80)
        v >>= 7
    return bytes(reversed(out))


def build_smf() -> bytes:
    track_body = bytearray()
    prev_tick = 0
    for (tick, status, d1, d2) in DEMO_EVENTS:
        track_body += write_vlq(tick - prev_tick)
        track_body += bytes([status, d1, d2])
        prev_tick = tick
    # End-of-track at the bar boundary.
    track_body += write_vlq(BAR_LENGTH_TICKS - prev_tick)
    track_body += b"\xFF\x2F\x00"

    mthd = b"MThd" + struct.pack(">IHHH", 6, 0, 1, PPQ)
    mtrk = b"MTrk" + struct.pack(">I", len(track_body)) + bytes(track_body)
    return mthd + mtrk


def write_groove_mid(path: Path) -> None:
    path.write_bytes(build_smf())


# ── Entry point ─────────────────────────────────────────────────────────────

if __name__ == "__main__":
    beep_path = SAMPLES / "beep.wav"
    groove_path = SONGS / "groove.mid"
    write_beep_wav(beep_path)
    write_groove_mid(groove_path)
    print(f"wrote {beep_path} ({beep_path.stat().st_size} bytes)")
    print(f"wrote {groove_path} ({groove_path.stat().st_size} bytes)")
