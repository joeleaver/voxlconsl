//! Minimal RIFF WAVE parser for the bundler.
//!
//! Accepts:
//! - mono or stereo (stereo is downmixed by averaging L/R)
//! - 8-bit unsigned or 16-bit signed PCM (format tag 1)
//! - 11025 or 22050 Hz sample rate (the engine's only two rates)
//! - optional `smpl` chunk for loop points (uses the first loop record)
//!
//! Anything else is rejected with a clear error string. We don't do
//! resampling — carts authoring samples must produce 11025/22050 at
//! the source.

use crate::BundleError;

/// PCM result from a parsed `.wav` file.
#[derive(Debug, Clone)]
pub struct WavPcm {
    pub sample_rate_hz: u32,
    /// 8-bit unsigned PCM, mono. (128 = silence.)
    pub pcm: Vec<u8>,
    /// First `smpl` loop record, if present. `(start_frame, end_frame)`
    /// where end is exclusive (one past the last looped frame, matching
    /// the audio engine's convention).
    pub loop_points: Option<(u32, u32)>,
}

const FMT_PCM: u16 = 1;

pub fn parse(bytes: &[u8]) -> Result<WavPcm, BundleError> {
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err(err("not a RIFF WAVE file"));
    }
    let total_len = u32::from_le_bytes(bytes[4..8].try_into().unwrap()) as usize;
    // total_len is "size of everything after the first 8 bytes". Clamp
    // to the actual buffer length so a fudged size header doesn't drop
    // us into UB on read.
    let body_end = core::cmp::min(8 + total_len, bytes.len());

    let mut cursor = 12usize;
    let mut sample_rate: Option<u32> = None;
    let mut channels: Option<u16> = None;
    let mut bits_per_sample: Option<u16> = None;
    let mut pcm_data: Option<&[u8]> = None;
    let mut loop_points: Option<(u32, u32)> = None;

    while cursor + 8 <= body_end {
        let id = &bytes[cursor..cursor + 4];
        let size = u32::from_le_bytes(bytes[cursor + 4..cursor + 8].try_into().unwrap()) as usize;
        let payload_start = cursor + 8;
        let payload_end = payload_start + size;
        if payload_end > bytes.len() {
            return Err(err("chunk extends past end of file"));
        }
        let payload = &bytes[payload_start..payload_end];

        match id {
            b"fmt " => {
                if payload.len() < 16 {
                    return Err(err("`fmt ` chunk too small"));
                }
                let format_tag = u16::from_le_bytes(payload[0..2].try_into().unwrap());
                if format_tag != FMT_PCM {
                    return Err(err("only PCM (format tag 1) is supported"));
                }
                channels = Some(u16::from_le_bytes(payload[2..4].try_into().unwrap()));
                sample_rate = Some(u32::from_le_bytes(payload[4..8].try_into().unwrap()));
                bits_per_sample = Some(u16::from_le_bytes(payload[14..16].try_into().unwrap()));
            }
            b"data" => {
                pcm_data = Some(payload);
            }
            b"smpl" => {
                // smpl chunk layout (Microsoft, in MIDIManufacturer.doc):
                //   00 manufacturer (u32)
                //   04 product
                //   08 sample_period
                //   12 midi_unity_note
                //   16 midi_pitch_fraction
                //   20 smpte_format
                //   24 smpte_offset
                //   28 num_sample_loops (u32)
                //   32 sampler_data
                //   36 loops: 24 bytes each
                //       00 cue_point_id
                //       04 type (0 = loop forward)
                //       08 start (frame index)
                //       12 end   (frame index, inclusive)
                //       16 fraction
                //       20 play_count
                if payload.len() >= 36 + 24 {
                    let num_loops =
                        u32::from_le_bytes(payload[28..32].try_into().unwrap());
                    if num_loops > 0 {
                        let lp = &payload[36..36 + 24];
                        let start = u32::from_le_bytes(lp[8..12].try_into().unwrap());
                        let end_inclusive =
                            u32::from_le_bytes(lp[12..16].try_into().unwrap());
                        // Engine convention: loop_end is exclusive (one
                        // past the last frame). RIFF smpl is inclusive,
                        // so bump by one.
                        loop_points = Some((start, end_inclusive.saturating_add(1)));
                    }
                }
            }
            _ => {
                // Tolerate unknown chunks ("LIST", "id3 ", "fact", etc).
            }
        }

        // Chunks are padded to a 2-byte boundary on disk.
        cursor = payload_end + (payload_end & 1);
    }

    let sample_rate = sample_rate.ok_or_else(|| err("missing `fmt ` chunk"))?;
    let channels = channels.ok_or_else(|| err("missing channel count"))?;
    let bits_per_sample =
        bits_per_sample.ok_or_else(|| err("missing bits-per-sample"))?;
    let pcm_data = pcm_data.ok_or_else(|| err("missing `data` chunk"))?;

    if sample_rate != 11_025 && sample_rate != 22_050 {
        return Err(err_owned(format!(
            "sample rate {sample_rate} Hz not supported (use 11025 or 22050)"
        )));
    }
    if channels != 1 && channels != 2 {
        return Err(err_owned(format!(
            "{channels}-channel WAV not supported (use mono or stereo)"
        )));
    }
    if bits_per_sample != 8 && bits_per_sample != 16 {
        return Err(err_owned(format!(
            "{bits_per_sample}-bit PCM not supported (use 8 or 16)"
        )));
    }

    let pcm_mono_u8 = decode_pcm(pcm_data, channels, bits_per_sample)?;

    Ok(WavPcm {
        sample_rate_hz: sample_rate,
        pcm: pcm_mono_u8,
        loop_points,
    })
}

fn decode_pcm(data: &[u8], channels: u16, bits: u16) -> Result<Vec<u8>, BundleError> {
    let bytes_per_sample = (bits as usize) / 8;
    let bytes_per_frame = bytes_per_sample * channels as usize;
    if bytes_per_frame == 0 {
        return Err(err("invalid frame size"));
    }
    let frame_count = data.len() / bytes_per_frame;
    let mut out = Vec::with_capacity(frame_count);

    for frame in 0..frame_count {
        let off = frame * bytes_per_frame;
        let sample_u8 = match (bits, channels) {
            (8, 1) => data[off],
            (8, 2) => {
                let l = data[off] as i32;
                let r = data[off + 1] as i32;
                ((l + r) / 2) as u8
            }
            (16, 1) => {
                let s = i16::from_le_bytes(data[off..off + 2].try_into().unwrap());
                i16_to_u8(s)
            }
            (16, 2) => {
                let l = i16::from_le_bytes(data[off..off + 2].try_into().unwrap()) as i32;
                let r = i16::from_le_bytes(data[off + 2..off + 4].try_into().unwrap()) as i32;
                i16_to_u8(((l + r) / 2) as i16)
            }
            _ => unreachable!("rejected upstream"),
        };
        out.push(sample_u8);
    }
    Ok(out)
}

/// Map a signed 16-bit sample to the engine's 8-bit unsigned convention
/// (128 = silence). Clamps + rounds toward zero.
fn i16_to_u8(s: i16) -> u8 {
    let centered = (s as i32 >> 8) + 128;
    centered.clamp(0, 255) as u8
}

fn err(msg: &'static str) -> BundleError {
    BundleError::AssetParse(msg.into())
}

fn err_owned(msg: String) -> BundleError {
    BundleError::AssetParse(msg)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal RIFF WAVE: 8-bit unsigned, mono, given sample rate
    /// and PCM bytes. No optional chunks.
    fn build_wav_mono_u8(rate: u32, pcm: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(b"RIFF");
        let body_size = 4 + (8 + 16) + (8 + pcm.len());
        out.extend_from_slice(&(body_size as u32).to_le_bytes());
        out.extend_from_slice(b"WAVE");
        // fmt
        out.extend_from_slice(b"fmt ");
        out.extend_from_slice(&16u32.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes()); // PCM
        out.extend_from_slice(&1u16.to_le_bytes()); // channels
        out.extend_from_slice(&rate.to_le_bytes());
        let byte_rate = rate;
        out.extend_from_slice(&byte_rate.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes()); // block align
        out.extend_from_slice(&8u16.to_le_bytes()); // bits
        // data
        out.extend_from_slice(b"data");
        out.extend_from_slice(&(pcm.len() as u32).to_le_bytes());
        out.extend_from_slice(pcm);
        out
    }

    #[test]
    fn parses_mono_8bit_22050() {
        let pcm = vec![100u8, 120, 140, 160];
        let bytes = build_wav_mono_u8(22_050, &pcm);
        let parsed = parse(&bytes).unwrap();
        assert_eq!(parsed.sample_rate_hz, 22_050);
        assert_eq!(parsed.pcm, pcm);
        assert!(parsed.loop_points.is_none());
    }

    #[test]
    fn rejects_unsupported_rate() {
        let bytes = build_wav_mono_u8(44_100, &[128, 128]);
        let err = parse(&bytes).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("44100"));
    }

    #[test]
    fn downmixes_stereo_to_mono() {
        // 16-bit stereo: 4 bytes/frame. 2 frames.
        // Frame 0: L=+10000, R=+10000 → average +10000 → ~+39 → 128+39=167
        // Frame 1: L=-10000, R=-10000 → average -10000 → ~-39 → 128-39=89
        let mut out = Vec::new();
        out.extend_from_slice(b"RIFF");
        let body_size = 4 + (8 + 16) + (8 + 8);
        out.extend_from_slice(&(body_size as u32).to_le_bytes());
        out.extend_from_slice(b"WAVE");
        out.extend_from_slice(b"fmt ");
        out.extend_from_slice(&16u32.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes()); // PCM
        out.extend_from_slice(&2u16.to_le_bytes()); // stereo
        out.extend_from_slice(&22_050u32.to_le_bytes());
        out.extend_from_slice(&(22_050u32 * 4).to_le_bytes()); // byte rate
        out.extend_from_slice(&4u16.to_le_bytes()); // block align
        out.extend_from_slice(&16u16.to_le_bytes()); // bits
        out.extend_from_slice(b"data");
        out.extend_from_slice(&8u32.to_le_bytes());
        out.extend_from_slice(&10000i16.to_le_bytes());
        out.extend_from_slice(&10000i16.to_le_bytes());
        out.extend_from_slice(&(-10000i16).to_le_bytes());
        out.extend_from_slice(&(-10000i16).to_le_bytes());
        let parsed = parse(&out).unwrap();
        assert_eq!(parsed.pcm.len(), 2);
        assert!(parsed.pcm[0] > 128 && parsed.pcm[0] < 200);
        assert!(parsed.pcm[1] < 128 && parsed.pcm[1] > 50);
    }

    #[test]
    fn parses_committed_big_world_beep_wav() {
        // Sanity-check that the asset committed alongside big-world
        // round-trips through the bundler. Catches drift between
        // `build_assets.py`'s encoder and `wav::parse`.
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../examples/big-world/audio/samples/beep.wav");
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(_) => return, // file is regenerated by the cart author; tolerate absence
        };
        let parsed = parse(&bytes).unwrap();
        assert_eq!(parsed.sample_rate_hz, 22_050);
        assert_eq!(parsed.pcm.len(), 4096);
        assert_eq!(parsed.loop_points, Some((64, 4032)));
    }

    #[test]
    fn reads_smpl_loop_points() {
        // Build a WAV with a smpl chunk having one forward loop.
        let pcm = vec![128u8; 100];
        let mut out = Vec::new();
        out.extend_from_slice(b"RIFF");
        let smpl_size = 36 + 24;
        let body_size = 4 + (8 + 16) + (8 + pcm.len()) + (8 + smpl_size);
        out.extend_from_slice(&(body_size as u32).to_le_bytes());
        out.extend_from_slice(b"WAVE");
        // fmt
        out.extend_from_slice(b"fmt ");
        out.extend_from_slice(&16u32.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&22_050u32.to_le_bytes());
        out.extend_from_slice(&22_050u32.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&8u16.to_le_bytes());
        // data
        out.extend_from_slice(b"data");
        out.extend_from_slice(&(pcm.len() as u32).to_le_bytes());
        out.extend_from_slice(&pcm);
        // smpl
        out.extend_from_slice(b"smpl");
        out.extend_from_slice(&(smpl_size as u32).to_le_bytes());
        out.extend_from_slice(&[0u8; 28]); // manufacturer..sampler_data prefix
        out.extend_from_slice(&1u32.to_le_bytes()); // num loops
        out.extend_from_slice(&0u32.to_le_bytes()); // sampler_data
        // loop record
        out.extend_from_slice(&0u32.to_le_bytes()); // cue id
        out.extend_from_slice(&0u32.to_le_bytes()); // type
        out.extend_from_slice(&10u32.to_le_bytes()); // start
        out.extend_from_slice(&80u32.to_le_bytes()); // end (inclusive)
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        let parsed = parse(&out).unwrap();
        assert_eq!(parsed.loop_points, Some((10, 81)));
    }
}
