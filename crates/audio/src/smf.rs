//! Standard MIDI File parser — see SPEC.md §5.3.
//!
//! Stage 4a (v0.1.12) input format. Parses SMF type 0 (single track)
//! and type 1 (multi-track, sync'd) into a flat, tempo-resolved event
//! list sorted by absolute tick. Type 2 (sequential independent
//! sequences) is rejected — it doesn't fit our single-playhead model.
//!
//! What we keep:
//! - Channel messages we recognize per §5.2: NoteOn, NoteOff,
//!   PitchBend, CC, ProgramChange. PolyAftertouch (0xA_) and
//!   ChannelPressure (0xD_) are silently dropped — the spec's CC
//!   table doesn't surface them.
//! - Meta SetTempo (FF 51 03) — drives the tick→time conversion at
//!   playback time. EndOfTrack (FF 2F 00) truncates a track. Other
//!   meta events (TimeSig, KeySig, Marker, lyrics, …) have no
//!   audible effect in our mixer and are skipped.
//! - SysEx is skipped.
//!
//! Running status is honored. SMPTE division (negative format) is
//! rejected — fantasy-console MIDI exports use PPQ in practice.

extern crate alloc;

use alloc::vec::Vec;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SmfError {
    /// Premature end-of-file.
    Truncated,
    /// MThd / MTrk chunk header missing or wrong.
    BadMagic,
    /// SMF format 2 (sequential sequences) is not supported.
    UnsupportedFormat,
    /// Division header uses SMPTE timecode, which we don't support.
    SmpteDivision,
    /// Status byte didn't decode into a known channel/meta/sysex event.
    BadEvent,
    /// Variable-length quantity ran past 4 bytes.
    BadVlq,
}

/// MIDI event in a parsed SMF. Subset of §5.2's recognized messages
/// plus SetTempo (the only meta event the playback engine acts on).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MidiEvent {
    NoteOn { channel: u8, note: u8, velocity: u8 },
    NoteOff { channel: u8, note: u8 },
    /// 14-bit pitch bend, centered at 0. Range -8192..=8191.
    PitchBend { channel: u8, value: i16 },
    Cc { channel: u8, controller: u8, value: u8 },
    ProgramChange { channel: u8, patch: u8 },
    /// Microseconds per quarter note. 500_000 = 120 BPM (the default).
    SetTempo { us_per_qn: u32 },
}

#[derive(Debug, Clone, Copy)]
pub struct TimedMidiEvent {
    /// Absolute tick from song start, in the song's `ticks_per_quarter`
    /// units. Multi-track type-1 files merge into a single timeline.
    pub tick: u32,
    pub event: MidiEvent,
}

/// Parsed SMF — ready to feed the playback engine.
#[derive(Debug, Clone)]
pub struct Song {
    pub events: Vec<TimedMidiEvent>,
    /// Division header — ticks per quarter note. Always ≥ 1.
    pub ticks_per_quarter: u16,
}

impl Song {
    /// Absolute tick of the last event. Empty songs return 0.
    pub fn end_tick(&self) -> u32 {
        self.events.last().map(|e| e.tick).unwrap_or(0)
    }

    /// Total quarter-note beats spanned. Useful for displaying song
    /// length without playing it back.
    pub fn end_beats(&self) -> f32 {
        self.end_tick() as f32 / self.ticks_per_quarter.max(1) as f32
    }
}

/// Parse an SMF byte stream. Returns a flat, sorted event list.
pub fn parse(bytes: &[u8]) -> Result<Song, SmfError> {
    let mut cur = Cursor::new(bytes);

    // ── MThd header ─────────────────────────────────────────────────
    let magic = cur.read_n(4)?;
    if magic != b"MThd" {
        return Err(SmfError::BadMagic);
    }
    let header_len = cur.read_u32_be()?;
    if header_len < 6 {
        return Err(SmfError::BadMagic);
    }
    let format = cur.read_u16_be()?;
    let ntracks = cur.read_u16_be()?;
    let division = cur.read_u16_be()?;
    // Skip extra bytes in the header chunk if the writer included any.
    if header_len > 6 {
        cur.advance((header_len - 6) as usize)?;
    }
    if format > 1 {
        return Err(SmfError::UnsupportedFormat);
    }
    if division & 0x8000 != 0 {
        return Err(SmfError::SmpteDivision);
    }
    let tpq = division.max(1);

    // ── MTrk chunks ─────────────────────────────────────────────────
    let mut all_events: Vec<TimedMidiEvent> = Vec::new();
    for _ in 0..ntracks {
        let magic = cur.read_n(4)?;
        if magic != b"MTrk" {
            return Err(SmfError::BadMagic);
        }
        let track_len = cur.read_u32_be()? as usize;
        let track_bytes = cur.read_n(track_len)?;
        parse_track(track_bytes, &mut all_events)?;
    }

    // Stable sort by absolute tick. Stable so intra-tick ordering
    // (e.g. ProgramChange before NoteOn at the same tick) is the order
    // tracks were written in the file.
    all_events.sort_by_key(|e| e.tick);

    Ok(Song { events: all_events, ticks_per_quarter: tpq })
}

fn parse_track(bytes: &[u8], out: &mut Vec<TimedMidiEvent>) -> Result<(), SmfError> {
    let mut cur = Cursor::new(bytes);
    let mut tick: u32 = 0;
    let mut running_status: u8 = 0;

    while cur.remaining() > 0 {
        let delta = cur.read_vlq()?;
        tick = tick.wrapping_add(delta);

        let first = cur.peek_u8()?;
        let status = if first & 0x80 != 0 {
            cur.advance(1)?;
            // Channel-message status bytes set running status; sysex
            // and meta events clear it (handled below).
            first
        } else {
            // Data byte at event boundary → running status applies.
            if running_status == 0 {
                return Err(SmfError::BadEvent);
            }
            running_status
        };

        match status {
            0xFF => {
                // Meta event clears running status per SMF spec.
                running_status = 0;
                let meta_type = cur.read_u8()?;
                let len = cur.read_vlq()? as usize;
                let data = cur.read_n(len)?;
                match meta_type {
                    0x51 if data.len() == 3 => {
                        let us = ((data[0] as u32) << 16)
                            | ((data[1] as u32) << 8)
                            | data[2] as u32;
                        out.push(TimedMidiEvent {
                            tick,
                            event: MidiEvent::SetTempo { us_per_qn: us },
                        });
                    }
                    0x2F => {
                        // End of track — stop parsing this track even
                        // if trailing bytes exist.
                        return Ok(());
                    }
                    _ => { /* ignore other meta types */ }
                }
            }
            0xF0 | 0xF7 => {
                // SysEx (start or continuation) — clears running status.
                running_status = 0;
                let len = cur.read_vlq()? as usize;
                cur.advance(len)?;
            }
            s if s & 0x80 != 0 => {
                // Channel voice message. Cache status for running status.
                running_status = s;
                let channel = s & 0x0F;
                match s & 0xF0 {
                    0x80 => {
                        let note = cur.read_u8()? & 0x7F;
                        let _vel = cur.read_u8()? & 0x7F;
                        out.push(TimedMidiEvent {
                            tick,
                            event: MidiEvent::NoteOff { channel, note },
                        });
                    }
                    0x90 => {
                        let note = cur.read_u8()? & 0x7F;
                        let vel = cur.read_u8()? & 0x7F;
                        let ev = if vel == 0 {
                            MidiEvent::NoteOff { channel, note }
                        } else {
                            MidiEvent::NoteOn { channel, note, velocity: vel }
                        };
                        out.push(TimedMidiEvent { tick, event: ev });
                    }
                    0xA0 => {
                        // Poly aftertouch — ignored (not in §5.2 CC table).
                        cur.advance(2)?;
                    }
                    0xB0 => {
                        let controller = cur.read_u8()? & 0x7F;
                        let value = cur.read_u8()? & 0x7F;
                        out.push(TimedMidiEvent {
                            tick,
                            event: MidiEvent::Cc { channel, controller, value },
                        });
                    }
                    0xC0 => {
                        let patch = cur.read_u8()? & 0x7F;
                        out.push(TimedMidiEvent {
                            tick,
                            event: MidiEvent::ProgramChange { channel, patch },
                        });
                    }
                    0xD0 => {
                        // Channel pressure — ignored.
                        cur.advance(1)?;
                    }
                    0xE0 => {
                        let lo = cur.read_u8()? & 0x7F;
                        let hi = cur.read_u8()? & 0x7F;
                        let raw = ((hi as i32) << 7) | lo as i32;
                        let bend = (raw - 8192) as i16;
                        out.push(TimedMidiEvent {
                            tick,
                            event: MidiEvent::PitchBend { channel, value: bend },
                        });
                    }
                    _ => return Err(SmfError::BadEvent),
                }
            }
            _ => return Err(SmfError::BadEvent),
        }
    }
    Ok(())
}

struct Cursor<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, pos: 0 }
    }
    fn remaining(&self) -> usize {
        self.bytes.len() - self.pos
    }
    fn advance(&mut self, n: usize) -> Result<(), SmfError> {
        if self.pos + n > self.bytes.len() {
            return Err(SmfError::Truncated);
        }
        self.pos += n;
        Ok(())
    }
    fn peek_u8(&self) -> Result<u8, SmfError> {
        self.bytes.get(self.pos).copied().ok_or(SmfError::Truncated)
    }
    fn read_u8(&mut self) -> Result<u8, SmfError> {
        let b = self.peek_u8()?;
        self.pos += 1;
        Ok(b)
    }
    fn read_n(&mut self, n: usize) -> Result<&'a [u8], SmfError> {
        if self.pos + n > self.bytes.len() {
            return Err(SmfError::Truncated);
        }
        let s = &self.bytes[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }
    fn read_u16_be(&mut self) -> Result<u16, SmfError> {
        let s = self.read_n(2)?;
        Ok(((s[0] as u16) << 8) | s[1] as u16)
    }
    fn read_u32_be(&mut self) -> Result<u32, SmfError> {
        let s = self.read_n(4)?;
        Ok(((s[0] as u32) << 24)
            | ((s[1] as u32) << 16)
            | ((s[2] as u32) << 8)
            | s[3] as u32)
    }
    /// Variable-length quantity per the MIDI spec: 7-bit groups, MSB
    /// signals continuation. Bounded at 4 bytes (max value 0x0FFF_FFFF).
    fn read_vlq(&mut self) -> Result<u32, SmfError> {
        let mut value: u32 = 0;
        for _ in 0..4 {
            let b = self.read_u8()?;
            value = (value << 7) | (b & 0x7F) as u32;
            if b & 0x80 == 0 {
                return Ok(value);
            }
        }
        Err(SmfError::BadVlq)
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    extern crate std;
    use std::vec;

    fn vlq(mut v: u32) -> Vec<u8> {
        // Encode `v` as a SMF VLQ (big-endian 7-bit, MSB = continuation).
        let mut bytes: Vec<u8> = vec![(v & 0x7F) as u8];
        v >>= 7;
        while v > 0 {
            bytes.insert(0, ((v & 0x7F) | 0x80) as u8);
            v >>= 7;
        }
        bytes
    }

    fn build_header(format: u16, ntrk: u16, division: u16) -> Vec<u8> {
        let mut h = b"MThd".to_vec();
        h.extend(&6u32.to_be_bytes());
        h.extend(&format.to_be_bytes());
        h.extend(&ntrk.to_be_bytes());
        h.extend(&division.to_be_bytes());
        h
    }

    fn build_track(events: &[(u32, &[u8])]) -> Vec<u8> {
        // events: (delta, raw event bytes including status byte).
        let mut body: Vec<u8> = Vec::new();
        for (d, e) in events {
            body.extend(vlq(*d));
            body.extend_from_slice(e);
        }
        // End-of-track meta
        body.extend(vlq(0));
        body.extend(&[0xFF, 0x2F, 0x00]);
        let mut out = b"MTrk".to_vec();
        out.extend(&(body.len() as u32).to_be_bytes());
        out.extend(body);
        out
    }

    #[test]
    fn vlq_round_trip() {
        for &v in &[0u32, 0x40, 0x7F, 0x80, 0x2000, 0x3FFF, 0x10_0000, 0x0FFF_FFFF] {
            let enc = vlq(v);
            let mut cur = Cursor::new(&enc);
            assert_eq!(cur.read_vlq().unwrap(), v, "round trip for {v:#x}");
        }
    }

    #[test]
    fn rejects_bad_magic() {
        let bytes = b"NOPE\0\0\0\x06\0\0\0\x01\x01\xE0";
        assert!(matches!(parse(bytes), Err(SmfError::BadMagic)));
    }

    #[test]
    fn rejects_format_2() {
        let mut bytes = build_header(2, 1, 96);
        bytes.extend(build_track(&[]));
        assert!(matches!(parse(&bytes), Err(SmfError::UnsupportedFormat)));
    }

    #[test]
    fn rejects_smpte_division() {
        let mut bytes = build_header(0, 1, 0x8001);
        bytes.extend(build_track(&[]));
        assert!(matches!(parse(&bytes), Err(SmfError::SmpteDivision)));
    }

    #[test]
    fn parses_single_note() {
        let mut bytes = build_header(0, 1, 96);
        // tick 0: note on ch0 note60 vel100. tick 96 (1 beat later): note off.
        bytes.extend(build_track(&[
            (0, &[0x90, 60, 100]),
            (96, &[0x80, 60, 64]),
        ]));
        let song = parse(&bytes).unwrap();
        assert_eq!(song.ticks_per_quarter, 96);
        assert_eq!(song.events.len(), 2);
        assert!(matches!(
            song.events[0].event,
            MidiEvent::NoteOn { channel: 0, note: 60, velocity: 100 }
        ));
        assert_eq!(song.events[0].tick, 0);
        assert!(matches!(
            song.events[1].event,
            MidiEvent::NoteOff { channel: 0, note: 60 }
        ));
        assert_eq!(song.events[1].tick, 96);
    }

    #[test]
    fn note_on_velocity_zero_becomes_note_off() {
        let mut bytes = build_header(0, 1, 96);
        bytes.extend(build_track(&[(0, &[0x90, 60, 0])]));
        let song = parse(&bytes).unwrap();
        assert_eq!(song.events.len(), 1);
        assert!(matches!(
            song.events[0].event,
            MidiEvent::NoteOff { channel: 0, note: 60 }
        ));
    }

    #[test]
    fn running_status() {
        let mut bytes = build_header(0, 1, 96);
        // Explicit note on; then a *running-status* note on (no status byte).
        bytes.extend(build_track(&[
            (0, &[0x90, 60, 100]),
            (10, &[62, 100]),  // running status reuses 0x90
            (10, &[64, 100]),
        ]));
        let song = parse(&bytes).unwrap();
        assert_eq!(song.events.len(), 3);
        assert!(matches!(
            song.events[0].event,
            MidiEvent::NoteOn { note: 60, .. }
        ));
        assert!(matches!(
            song.events[1].event,
            MidiEvent::NoteOn { note: 62, .. }
        ));
        assert!(matches!(
            song.events[2].event,
            MidiEvent::NoteOn { note: 64, .. }
        ));
        assert_eq!(song.events[2].tick, 20);
    }

    #[test]
    fn set_tempo_meta() {
        let mut bytes = build_header(0, 1, 96);
        // FF 51 03 07 A1 20 = 500_000 us/qn (120 BPM)
        bytes.extend(build_track(&[
            (0, &[0xFF, 0x51, 0x03, 0x07, 0xA1, 0x20]),
            (48, &[0x90, 60, 100]),
        ]));
        let song = parse(&bytes).unwrap();
        assert!(matches!(
            song.events[0].event,
            MidiEvent::SetTempo { us_per_qn: 500_000 }
        ));
    }

    #[test]
    fn pitch_bend_centered_is_zero() {
        let mut bytes = build_header(0, 1, 96);
        // 0xE0 0x00 0x40 -> 14-bit value 0x2000 -> -8192 + 8192 = 0
        bytes.extend(build_track(&[(0, &[0xE0, 0x00, 0x40])]));
        let song = parse(&bytes).unwrap();
        assert!(matches!(
            song.events[0].event,
            MidiEvent::PitchBend { value: 0, .. }
        ));
    }

    #[test]
    fn cc_and_program_change() {
        let mut bytes = build_header(0, 1, 96);
        bytes.extend(build_track(&[
            (0, &[0xB0, 7, 100]),   // CC volume on ch0
            (0, &[0xC1, 5]),         // PC on ch1
        ]));
        let song = parse(&bytes).unwrap();
        assert!(matches!(
            song.events[0].event,
            MidiEvent::Cc { channel: 0, controller: 7, value: 100 }
        ));
        assert!(matches!(
            song.events[1].event,
            MidiEvent::ProgramChange { channel: 1, patch: 5 }
        ));
    }

    #[test]
    fn type_1_multi_track_merges_by_tick() {
        let mut bytes = build_header(1, 2, 96);
        bytes.extend(build_track(&[
            (0, &[0x90, 60, 100]),
            (96, &[0x80, 60, 0]),
        ]));
        bytes.extend(build_track(&[
            (48, &[0x91, 67, 100]),   // half-beat in on ch1
        ]));
        let song = parse(&bytes).unwrap();
        assert_eq!(song.events.len(), 3);
        // Merged-sorted by tick: 0, 48, 96.
        assert_eq!(song.events[0].tick, 0);
        assert_eq!(song.events[1].tick, 48);
        assert_eq!(song.events[2].tick, 96);
    }

    #[test]
    fn end_of_track_truncates() {
        let mut bytes = build_header(0, 1, 96);
        let mut track_body: Vec<u8> = Vec::new();
        track_body.extend(vlq(0));
        track_body.extend(&[0x90, 60, 100]);
        // EOT
        track_body.extend(vlq(0));
        track_body.extend(&[0xFF, 0x2F, 0x00]);
        // Trailing garbage that must be ignored.
        track_body.extend(&[0xDE, 0xAD, 0xBE, 0xEF]);
        bytes.extend(b"MTrk");
        bytes.extend(&(track_body.len() as u32).to_be_bytes());
        bytes.extend(&track_body);
        let song = parse(&bytes).unwrap();
        assert_eq!(song.events.len(), 1);
    }

    #[test]
    fn truncated_returns_error() {
        let bytes = b"MThd\0\0\0\x06\0";
        assert!(matches!(parse(bytes), Err(SmfError::Truncated)));
    }

    #[test]
    fn unsupported_running_status_at_start_is_rejected() {
        let mut bytes = build_header(0, 1, 96);
        // Track body starts with a data byte (no status established).
        let mut body: Vec<u8> = Vec::new();
        body.extend(vlq(0));
        body.push(60);  // data byte, no preceding status
        body.extend(vlq(0));
        body.extend(&[0xFF, 0x2F, 0x00]);
        bytes.extend(b"MTrk");
        bytes.extend(&(body.len() as u32).to_be_bytes());
        bytes.extend(&body);
        assert!(matches!(parse(&bytes), Err(SmfError::BadEvent)));
    }
}
