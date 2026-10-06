//! Audio preparation: decoding arbitrary files to 44.1 kHz stereo PCM, and
//! optional ATRAC3 (LP2/LP4) encoding through an external `atracdenc` binary.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, anyhow, bail};
use rubato::audioadapter_buffers::direct::InterleavedSlice;
use rubato::{Fft, FixedSync, Resampler};
use symphonia::core::codecs::audio::AudioDecoderOptions;
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;

use crate::model::UploadFormat;

pub const SAMPLE_RATE: u32 = 44_100;

/// Decode any supported file into interleaved stereo 16-bit PCM at 44.1 kHz.
pub fn decode_to_cd_pcm(path: &Path) -> Result<Vec<i16>> {
    let file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());

    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }

    let mut format = symphonia::default::get_probe()
        .probe(&hint, mss, FormatOptions::default(), MetadataOptions::default())
        .context("unsupported or unreadable audio file")?;

    let track = format
        .default_track(TrackType::Audio)
        .ok_or_else(|| anyhow!("file has no audio track"))?;
    let track_id = track.id;
    let params = track
        .codec_params
        .as_ref()
        .and_then(|p| p.audio())
        .ok_or_else(|| anyhow!("file has no audio codec parameters"))?;
    let mut decoder = symphonia::default::get_codecs()
        .make_audio_decoder(params, &AudioDecoderOptions::default())
        .context("no decoder for this codec")?;

    let mut rate = 0u32;
    let mut stereo: Vec<f32> = Vec::new();
    let mut packet_buf: Vec<f32> = Vec::new();

    while let Some(packet) = format.next_packet()? {
        if packet.track_id != track_id {
            continue;
        }
        let buf = match decoder.decode(&packet) {
            Ok(buf) => buf,
            Err(SymphoniaError::DecodeError(_)) => continue,
            Err(e) => return Err(e.into()),
        };
        rate = buf.spec().rate();
        let channels = buf.spec().channels().count().max(1);
        packet_buf.resize(buf.samples_interleaved(), 0.0);
        buf.copy_to_slice_interleaved(&mut packet_buf);

        // Fold everything into stereo: mono is duplicated, extra channels dropped.
        for frame in packet_buf.chunks_exact(channels) {
            let left = frame[0];
            let right = if channels > 1 { frame[1] } else { frame[0] };
            stereo.push(left);
            stereo.push(right);
        }
    }

    if stereo.is_empty() {
        bail!("file contains no audio");
    }

    let stereo = if rate != SAMPLE_RATE { resample(&stereo, rate)? } else { stereo };

    Ok(stereo
        .into_iter()
        .map(|s| (s.clamp(-1.0, 1.0) * i16::MAX as f32).round() as i16)
        .collect())
}

fn resample(stereo: &[f32], rate_in: u32) -> Result<Vec<f32>> {
    let frames_in = stereo.len() / 2;
    let mut resampler =
        Fft::<f32>::new(rate_in as usize, SAMPLE_RATE as usize, 1024, 2, FixedSync::Input)
            .map_err(|e| anyhow!("resampler: {e}"))?;

    let out_capacity = resampler.process_all_needed_output_len(frames_in);
    let mut out = vec![0f32; out_capacity * 2];

    let input = InterleavedSlice::new(stereo, 2, frames_in).map_err(|e| anyhow!("{e}"))?;
    let mut output =
        InterleavedSlice::new_mut(&mut out, 2, out_capacity).map_err(|e| anyhow!("{e}"))?;
    let (_, frames_out) = resampler
        .process_all_into_buffer(&input, &mut output, frames_in, None)
        .map_err(|e| anyhow!("resampling failed: {e}"))?;

    out.truncate(frames_out * 2);
    Ok(out)
}

/// NetMD expects SP-mode PCM as big-endian signed 16-bit stereo.
pub fn pcm_to_netmd_bytes(pcm: &[i16]) -> Vec<u8> {
    pcm.iter().flat_map(|s| s.to_be_bytes()).collect()
}

/// Find an `atracdenc` binary: `$ATRACDENC`, next to our executable, or `$PATH`.
pub fn find_atracdenc() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("ATRACDENC").map(PathBuf::from)
        && p.is_file()
    {
        return Some(p);
    }
    let exe_name = if cfg!(windows) { "atracdenc.exe" } else { "atracdenc" };
    if let Some(dir) = std::env::current_exe().ok().and_then(|e| e.parent().map(Path::to_path_buf))
    {
        let candidate = dir.join(exe_name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths).map(|d| d.join(exe_name)).find(|p| p.is_file())
    })
}

/// Encode PCM to raw ATRAC3 frames (LP2: 384 B/frame, LP4: 192 B/frame).
pub fn encode_atrac3(pcm: &[i16], format: UploadFormat) -> Result<Vec<u8>> {
    let codec = match format {
        UploadFormat::Lp2 => "atrac3",
        UploadFormat::Lp4 => "atrac3_lp4",
        UploadFormat::Sp => bail!("SP does not need host-side encoding"),
    };
    let encoder = find_atracdenc().ok_or_else(|| {
        anyhow!("LP2/LP4 needs `atracdenc` on your PATH (or set $ATRACDENC)")
    })?;

    let dir = tempfile::tempdir()?;
    let wav_path = dir.path().join("in.wav");
    let raw_path = dir.path().join("out.raw");
    std::fs::write(&wav_path, pcm_wav(pcm))?;

    let output = Command::new(&encoder)
        .arg(format!("--encode={codec}"))
        .arg("--container=raw")
        .arg("--nostdout")
        .arg("-i")
        .arg(&wav_path)
        .arg("-o")
        .arg(&raw_path)
        .output()
        .with_context(|| format!("running {}", encoder.display()))?;
    if !output.status.success() {
        bail!("atracdenc failed: {}", String::from_utf8_lossy(&output.stderr).trim());
    }
    Ok(std::fs::read(&raw_path)?)
}

/// A canonical 16-bit stereo 44.1 kHz little-endian WAV file.
fn pcm_wav(pcm: &[i16]) -> Vec<u8> {
    let data_len = (pcm.len() * 2) as u32;
    let mut out = Vec::with_capacity(44 + data_len as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&2u16.to_le_bytes()); // channels
    out.extend_from_slice(&SAMPLE_RATE.to_le_bytes());
    out.extend_from_slice(&(SAMPLE_RATE * 4).to_le_bytes()); // byte rate
    out.extend_from_slice(&4u16.to_le_bytes()); // block align
    out.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for s in pcm {
        out.extend_from_slice(&s.to_le_bytes());
    }
    out
}

/// Header for an ATRAC1 `.aea` file (SP tracks read back from an MZ-RH1).
pub fn aea_header(name: &str, channels: u8, sound_groups: u32) -> Vec<u8> {
    let mut header = vec![0u8; 2048];
    header[0..4].copy_from_slice(&2048u32.to_le_bytes());
    let name = name.as_bytes();
    let n = name.len().min(255);
    header[4..4 + n].copy_from_slice(&name[..n]);
    header[260..264].copy_from_slice(&sound_groups.to_le_bytes());
    header[264] = channels;
    header
}

/// RIFF/WAVE header wrapping raw ATRAC3 data (LP tracks read back from an MZ-RH1).
pub fn atrac3_wav_header(lp4: bool, data_len: u32) -> Vec<u8> {
    let block_align: u16 = if lp4 { 192 } else { 384 };
    let joint_stereo: u16 = if lp4 { 1 } else { 0 };
    let byte_rate = block_align as u32 * SAMPLE_RATE / 1024;

    let mut h = Vec::with_capacity(68);
    h.extend_from_slice(b"RIFF");
    h.extend_from_slice(&(60 + data_len).to_le_bytes());
    h.extend_from_slice(b"WAVEfmt ");
    h.extend_from_slice(&32u32.to_le_bytes());
    h.extend_from_slice(&0x0270u16.to_le_bytes()); // WAVE_FORMAT_SONY_SCX (ATRAC3)
    h.extend_from_slice(&2u16.to_le_bytes());
    h.extend_from_slice(&SAMPLE_RATE.to_le_bytes());
    h.extend_from_slice(&byte_rate.to_le_bytes());
    h.extend_from_slice(&block_align.to_le_bytes());
    h.extend_from_slice(&0u16.to_le_bytes()); // bits per sample
    h.extend_from_slice(&14u16.to_le_bytes()); // extra size
    h.extend_from_slice(&1u16.to_le_bytes());
    h.extend_from_slice(&0x0800u32.to_le_bytes()); // samples per frame (1024 * 2 ch)
    h.extend_from_slice(&joint_stereo.to_le_bytes());
    h.extend_from_slice(&joint_stereo.to_le_bytes());
    h.extend_from_slice(&1u16.to_le_bytes());
    h.extend_from_slice(&0u16.to_le_bytes());
    h.extend_from_slice(b"data");
    h.extend_from_slice(&data_len.to_le_bytes());
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_and_resamples_wav() {
        // One second of 48 kHz mono silence-with-a-ramp written as a WAV.
        let rate = 48_000u32;
        let samples: Vec<i16> = (0..rate as i32).map(|i| (i % 1000) as i16).collect();
        let mut wav = Vec::new();
        let data_len = (samples.len() * 2) as u32;
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&(36 + data_len).to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&rate.to_le_bytes());
        wav.extend_from_slice(&(rate * 2).to_le_bytes());
        wav.extend_from_slice(&2u16.to_le_bytes());
        wav.extend_from_slice(&16u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&data_len.to_le_bytes());
        for s in &samples {
            wav.extend_from_slice(&s.to_le_bytes());
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.wav");
        std::fs::write(&path, wav).unwrap();

        let pcm = decode_to_cd_pcm(&path).unwrap();
        let frames = pcm.len() / 2;
        // ~1 s at 44.1 kHz, allowing for resampler edge handling.
        assert!((frames as i64 - 44_100).abs() < 1_100, "got {frames} frames");
        // Mono was duplicated to both channels.
        assert_eq!(pcm[2000], pcm[2001]);
    }

    #[test]
    fn netmd_pcm_is_big_endian() {
        assert_eq!(pcm_to_netmd_bytes(&[0x0102, -2]), vec![0x01, 0x02, 0xff, 0xfe]);
    }
}
