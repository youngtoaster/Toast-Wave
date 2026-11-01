//! Broadcast Wave Format (BWF) parser and writer for professional DAW audio interchange.
//!
//! Extracts the 64-bit `TimeReference` from the `bext` chunk to determine exact timeline
//! sample offsets, and writes BWF `bext` chunks into WAV files so other DAWs (Logic Pro,
//! Pro Tools, Reaper, Ableton, FL Studio, Cubase, Studio One) can snap stems to their
//! exact project locations.

use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::Path;

/// Metadata extracted from a WAV / BWF file.
#[derive(Clone, Debug, PartialEq)]
pub struct BwfInfo {
    pub sample_rate: u32,
    pub channels: u16,
    pub bits_per_sample: u16,
    /// 64-bit sample offset from project start (from 'bext' TimeReference).
    pub time_reference: Option<u64>,
    /// Total audio sample frames in the file.
    pub total_samples: u64,
    /// Total duration in seconds.
    pub duration_seconds: f64,
    /// Timeline start offset in seconds: `time_reference / sample_rate`.
    pub start_seconds: Option<f64>,
    pub description: String,
    pub originator: String,
}

/// Reads BWF metadata from a WAV file.
pub fn read_bwf_info(path: &Path) -> Result<BwfInfo, String> {
    let file = File::open(path).map_err(|e| format!("cannot open {}: {e}", path.display()))?;
    let mut reader = BufReader::new(file);

    let mut header = [0u8; 12];
    reader
        .read_exact(&mut header)
        .map_err(|e| format!("failed to read header: {e}"))?;

    if &header[0..4] != b"RIFF" || &header[8..12] != b"WAVE" {
        return Err("not a RIFF/WAVE file".to_string());
    }

    let mut sample_rate: Option<u32> = None;
    let mut channels: Option<u16> = None;
    let mut bits_per_sample: Option<u16> = None;
    let mut time_reference: Option<u64> = None;
    let mut total_samples: u64 = 0;
    let mut description = String::new();
    let mut originator = String::new();

    let mut chunk_hdr = [0u8; 8];
    while reader.read_exact(&mut chunk_hdr).is_ok() {
        let chunk_id = &chunk_hdr[0..4];
        let chunk_size = u32::from_le_bytes([
            chunk_hdr[4],
            chunk_hdr[5],
            chunk_hdr[6],
            chunk_hdr[7],
        ]) as u64;

        match chunk_id {
            b"fmt " => {
                let to_read = (chunk_size as usize).min(40);
                let mut fmt_buf = vec![0u8; to_read];
                reader
                    .read_exact(&mut fmt_buf)
                    .map_err(|e| format!("failed reading fmt chunk: {e}"))?;

                if fmt_buf.len() >= 16 {
                    channels = Some(u16::from_le_bytes([fmt_buf[2], fmt_buf[3]]));
                    sample_rate = Some(u32::from_le_bytes([
                        fmt_buf[4], fmt_buf[5], fmt_buf[6], fmt_buf[7],
                    ]));
                    bits_per_sample = Some(u16::from_le_bytes([fmt_buf[14], fmt_buf[15]]));
                }

                if chunk_size > to_read as u64 {
                    reader
                        .seek(SeekFrom::Current((chunk_size - to_read as u64) as i64))
                        .map_err(|e| format!("seek error: {e}"))?;
                }
            }
            b"bext" => {
                let to_read = chunk_size.min(1024) as usize;
                let mut bext_buf = vec![0u8; to_read];
                reader
                    .read_exact(&mut bext_buf)
                    .map_err(|e| format!("failed reading bext chunk: {e}"))?;

                if bext_buf.len() >= 256 {
                    description = String::from_utf8_lossy(&bext_buf[0..256])
                        .trim_matches('\0')
                        .trim()
                        .to_string();
                }
                if bext_buf.len() >= 288 {
                    originator = String::from_utf8_lossy(&bext_buf[256..288])
                        .trim_matches('\0')
                        .trim()
                        .to_string();
                }
                if bext_buf.len() >= 346 {
                    let low = u32::from_le_bytes([
                        bext_buf[338],
                        bext_buf[339],
                        bext_buf[340],
                        bext_buf[341],
                    ]) as u64;
                    let high = u32::from_le_bytes([
                        bext_buf[342],
                        bext_buf[343],
                        bext_buf[344],
                        bext_buf[345],
                    ]) as u64;
                    time_reference = Some((high << 32) | low);
                }

                if chunk_size > to_read as u64 {
                    reader
                        .seek(SeekFrom::Current((chunk_size - to_read as u64) as i64))
                        .map_err(|e| format!("seek error: {e}"))?;
                }
            }
            b"data" => {
                let bytes_per_frame = match (channels, bits_per_sample) {
                    (Some(ch), Some(bits)) if ch > 0 && bits > 0 => {
                        (ch as u64) * ((bits as u64 + 7) / 8)
                    }
                    _ => 4,
                };
                if bytes_per_frame > 0 {
                    total_samples = chunk_size / bytes_per_frame;
                }
                reader
                    .seek(SeekFrom::Current(chunk_size as i64))
                    .map_err(|e| format!("seek error: {e}"))?;
            }
            _ => {
                reader
                    .seek(SeekFrom::Current(chunk_size as i64))
                    .map_err(|e| format!("seek error: {e}"))?;
            }
        }

        // RIFF chunks are 2-byte aligned
        if chunk_size % 2 != 0 {
            let _ = reader.seek(SeekFrom::Current(1));
        }
    }

    let sr = sample_rate.unwrap_or(44100);
    let dur = if sr > 0 {
        total_samples as f64 / sr as f64
    } else {
        0.0
    };
    let start_sec = time_reference.map(|tr| tr as f64 / sr as f64);

    Ok(BwfInfo {
        sample_rate: sr,
        channels: channels.unwrap_or(2),
        bits_per_sample: bits_per_sample.unwrap_or(16),
        time_reference,
        total_samples,
        duration_seconds: dur,
        start_seconds: start_sec,
        description,
        originator,
    })
}

/// Builds a 602-byte BWF `bext` chunk payload.
pub fn build_bext_chunk(
    description: &str,
    originator: &str,
    time_reference: u64,
    date_str: Option<&str>,
    time_str: Option<&str>,
) -> Vec<u8> {
    let mut buf = vec![0u8; 602];

    // Description: 256 bytes
    let desc_bytes = description.as_bytes();
    let desc_len = desc_bytes.len().min(256);
    buf[0..desc_len].copy_from_slice(&desc_bytes[0..desc_len]);

    // Originator: 32 bytes
    let orig_bytes = originator.as_bytes();
    let orig_len = orig_bytes.len().min(32);
    buf[256..256 + orig_len].copy_from_slice(&orig_bytes[0..orig_len]);

    // OriginatorReference: 32 bytes
    let ref_str = format!("TW_{:016X}", time_reference);
    let ref_bytes = ref_str.as_bytes();
    let ref_len = ref_bytes.len().min(32);
    buf[288..288 + ref_len].copy_from_slice(&ref_bytes[0..ref_len]);

    // OriginationDate: 10 bytes (YYYY-MM-DD)
    let d = date_str.unwrap_or("2026-11-01");
    let d_bytes = d.as_bytes();
    let d_len = d_bytes.len().min(10);
    buf[320..320 + d_len].copy_from_slice(&d_bytes[0..d_len]);

    // OriginationTime: 8 bytes (HH:MM:SS)
    let t = time_str.unwrap_or("00:00:00");
    let t_bytes = t.as_bytes();
    let t_len = t_bytes.len().min(8);
    buf[330..330 + t_len].copy_from_slice(&t_bytes[0..t_len]);

    // TimeReference: 8 bytes (Low u32 LE, High u32 LE)
    let low = (time_reference & 0xFFFF_FFFF) as u32;
    let high = ((time_reference >> 32) & 0xFFFF_FFFF) as u32;
    buf[338..342].copy_from_slice(&low.to_le_bytes());
    buf[342..346].copy_from_slice(&high.to_le_bytes());

    // Version: 1 (u16 LE @ 346)
    buf[346..348].copy_from_slice(&1u16.to_le_bytes());

    // Remainder is UMID, Loudness, and Reserved (all zeroes is standard compliant)
    buf
}

/// Copies `src_wav` to `dst_wav`, ensuring it has a valid BWF `bext` chunk with `time_reference`.
pub fn write_bwf_copy(
    src_wav: &Path,
    dst_wav: &Path,
    time_reference: u64,
    description: &str,
    originator: &str,
) -> Result<(), String> {
    let src_file =
        File::open(src_wav).map_err(|e| format!("cannot open source {}: {e}", src_wav.display()))?;
    let mut reader = BufReader::new(src_file);

    let mut header = [0u8; 12];
    reader
        .read_exact(&mut header)
        .map_err(|e| format!("read header error: {e}"))?;
    if &header[0..4] != b"RIFF" || &header[8..12] != b"WAVE" {
        return Err("source is not a RIFF/WAVE file".to_string());
    }

    // Read all existing chunks except any existing `bext` chunk
    let mut chunks: Vec<([u8; 4], Vec<u8>)> = Vec::new();
    let mut chunk_hdr = [0u8; 8];
    while reader.read_exact(&mut chunk_hdr).is_ok() {
        let chunk_id: [u8; 4] = chunk_hdr[0..4].try_into().unwrap_or([0; 4]);
        let chunk_size = u32::from_le_bytes([
            chunk_hdr[4],
            chunk_hdr[5],
            chunk_hdr[6],
            chunk_hdr[7],
        ]) as usize;

        let mut data = vec![0u8; chunk_size];
        reader
            .read_exact(&mut data)
            .map_err(|e| format!("read chunk {}: {e}", String::from_utf8_lossy(&chunk_id)))?;

        if chunk_size % 2 != 0 {
            let mut pad = [0u8; 1];
            let _ = reader.read_exact(&mut pad);
        }

        if &chunk_id != b"bext" {
            chunks.push((chunk_id, data));
        }
    }

    let bext_data = build_bext_chunk(description, originator, time_reference, None, None);

    // Calculate new total RIFF size: 4 (WAVE) + bext header (8) + bext data (602) + chunks...
    let mut body_size = 4 + 8 + bext_data.len();
    for (_, data) in &chunks {
        let pad = if data.len() % 2 != 0 { 1 } else { 0 };
        body_size += 8 + data.len() + pad;
    }

    let dst_file = File::create(dst_wav)
        .map_err(|e| format!("cannot create target {}: {e}", dst_wav.display()))?;
    let mut writer = BufWriter::new(dst_file);

    // RIFF header
    writer.write_all(b"RIFF").map_err(|e| e.to_string())?;
    writer
        .write_all(&(body_size as u32).to_le_bytes())
        .map_err(|e| e.to_string())?;
    writer.write_all(b"WAVE").map_err(|e| e.to_string())?;

    // Write bext chunk first
    writer.write_all(b"bext").map_err(|e| e.to_string())?;
    writer
        .write_all(&(bext_data.len() as u32).to_le_bytes())
        .map_err(|e| e.to_string())?;
    writer.write_all(&bext_data).map_err(|e| e.to_string())?;
    if bext_data.len() % 2 != 0 {
        writer.write_all(&[0u8]).map_err(|e| e.to_string())?;
    }

    // Write remaining chunks (fmt, data, etc.)
    for (id, data) in &chunks {
        writer.write_all(id).map_err(|e| e.to_string())?;
        writer
            .write_all(&(data.len() as u32).to_le_bytes())
            .map_err(|e| e.to_string())?;
        writer.write_all(data).map_err(|e| e.to_string())?;
        if data.len() % 2 != 0 {
            writer.write_all(&[0u8]).map_err(|e| e.to_string())?;
        }
    }

    writer.flush().map_err(|e| e.to_string())?;
    Ok(())
}

/// Creates a new BWF 24-bit or 16-bit stereo/mono WAV file from raw interleaved f32 PCM samples.
pub fn write_bwf_pcm(
    path: &Path,
    samples: &[f32],
    sample_rate: u32,
    channels: u16,
    bits_per_sample: u16,
    time_reference: u64,
    description: &str,
    originator: &str,
) -> Result<(), String> {
    let bytes_per_sample = ((bits_per_sample + 7) / 8) as usize;
    let block_align = channels as usize * bytes_per_sample;
    let byte_rate = sample_rate as usize * block_align;

    // Convert f32 samples (-1.0..=1.0) to integer PCM
    let mut pcm_bytes = Vec::with_capacity(samples.len() * bytes_per_sample);
    match bits_per_sample {
        16 => {
            for &s in samples {
                let clamped = s.clamp(-1.0, 1.0);
                let val = (clamped * 32767.0).round() as i16;
                pcm_bytes.extend_from_slice(&val.to_le_bytes());
            }
        }
        24 => {
            for &s in samples {
                let clamped = s.clamp(-1.0, 1.0);
                let val = (clamped * 8388607.0).round() as i32;
                let b = val.to_le_bytes();
                pcm_bytes.push(b[0]);
                pcm_bytes.push(b[1]);
                pcm_bytes.push(b[2]);
            }
        }
        32 => {
            // 32-bit float
            for &s in samples {
                pcm_bytes.extend_from_slice(&s.to_le_bytes());
            }
        }
        _ => return Err(format!("unsupported bit depth: {bits_per_sample}")),
    }

    let bext_data = build_bext_chunk(description, originator, time_reference, None, None);

    // fmt chunk payload (16 bytes for standard PCM, 18 for float/extensible)
    let format_tag: u16 = if bits_per_sample == 32 { 3 } else { 1 };
    let mut fmt_data = Vec::with_capacity(16);
    fmt_data.extend_from_slice(&format_tag.to_le_bytes());
    fmt_data.extend_from_slice(&channels.to_le_bytes());
    fmt_data.extend_from_slice(&sample_rate.to_le_bytes());
    fmt_data.extend_from_slice(&(byte_rate as u32).to_le_bytes());
    fmt_data.extend_from_slice(&(block_align as u16).to_le_bytes());
    fmt_data.extend_from_slice(&bits_per_sample.to_le_bytes());

    let data_len = pcm_bytes.len();
    let data_pad = if data_len % 2 != 0 { 1 } else { 0 };

    let riff_body_size = 4
        + (8 + bext_data.len())
        + (8 + fmt_data.len())
        + (8 + data_len + data_pad);

    let file = File::create(path).map_err(|e| format!("cannot create {}: {e}", path.display()))?;
    let mut writer = BufWriter::new(file);

    // RIFF Header
    writer.write_all(b"RIFF").map_err(|e| e.to_string())?;
    writer
        .write_all(&(riff_body_size as u32).to_le_bytes())
        .map_err(|e| e.to_string())?;
    writer.write_all(b"WAVE").map_err(|e| e.to_string())?;

    // bext chunk
    writer.write_all(b"bext").map_err(|e| e.to_string())?;
    writer
        .write_all(&(bext_data.len() as u32).to_le_bytes())
        .map_err(|e| e.to_string())?;
    writer.write_all(&bext_data).map_err(|e| e.to_string())?;

    // fmt chunk
    writer.write_all(b"fmt ").map_err(|e| e.to_string())?;
    writer
        .write_all(&(fmt_data.len() as u32).to_le_bytes())
        .map_err(|e| e.to_string())?;
    writer.write_all(&fmt_data).map_err(|e| e.to_string())?;

    // data chunk
    writer.write_all(b"data").map_err(|e| e.to_string())?;
    writer
        .write_all(&(data_len as u32).to_le_bytes())
        .map_err(|e| e.to_string())?;
    writer.write_all(&pcm_bytes).map_err(|e| e.to_string())?;
    if data_pad > 0 {
        writer.write_all(&[0u8]).map_err(|e| e.to_string())?;
    }

    writer.flush().map_err(|e| e.to_string())?;
    Ok(())
}

/// Reads interleaved f32 audio samples from a WAV file along with its sample rate and channel count.
pub fn read_pcm_f32(path: &Path) -> Result<(Vec<f32>, u32, u16), String> {
    let file = File::open(path).map_err(|e| format!("cannot open {}: {e}", path.display()))?;
    let mut reader = BufReader::new(file);

    let mut header = [0u8; 12];
    reader
        .read_exact(&mut header)
        .map_err(|e| format!("failed to read header: {e}"))?;

    if &header[0..4] != b"RIFF" || &header[8..12] != b"WAVE" {
        return Err("not a RIFF/WAVE file".to_string());
    }

    let mut audio_format: u16 = 1;
    let mut channels: u16 = 2;
    let mut sample_rate: u32 = 44100;
    let mut bits_per_sample: u16 = 16;
    let mut pcm_samples: Vec<f32> = Vec::new();

    let mut chunk_hdr = [0u8; 8];
    while reader.read_exact(&mut chunk_hdr).is_ok() {
        let chunk_id = &chunk_hdr[0..4];
        let chunk_size = u32::from_le_bytes([
            chunk_hdr[4],
            chunk_hdr[5],
            chunk_hdr[6],
            chunk_hdr[7],
        ]) as u64;

        match chunk_id {
            b"fmt " => {
                let to_read = (chunk_size as usize).min(40);
                let mut fmt_buf = vec![0u8; to_read];
                reader
                    .read_exact(&mut fmt_buf)
                    .map_err(|e| format!("failed reading fmt chunk: {e}"))?;

                if fmt_buf.len() >= 16 {
                    audio_format = u16::from_le_bytes([fmt_buf[0], fmt_buf[1]]);
                    channels = u16::from_le_bytes([fmt_buf[2], fmt_buf[3]]);
                    sample_rate = u32::from_le_bytes([
                        fmt_buf[4], fmt_buf[5], fmt_buf[6], fmt_buf[7],
                    ]);
                    bits_per_sample = u16::from_le_bytes([fmt_buf[14], fmt_buf[15]]);
                }

                if chunk_size > to_read as u64 {
                    reader
                        .seek(SeekFrom::Current((chunk_size - to_read as u64) as i64))
                        .map_err(|e| format!("seek error: {e}"))?;
                }
            }
            b"data" => {
                let mut data_buf = vec![0u8; chunk_size as usize];
                reader
                    .read_exact(&mut data_buf)
                    .map_err(|e| format!("failed reading data chunk: {e}"))?;

                match (audio_format, bits_per_sample) {
                    (1, 16) => {
                        let sample_count = data_buf.len() / 2;
                        pcm_samples.reserve(sample_count);
                        for i in 0..sample_count {
                            let val = i16::from_le_bytes([data_buf[i * 2], data_buf[i * 2 + 1]]);
                            pcm_samples.push(val as f32 / 32768.0);
                        }
                    }
                    (1, 24) => {
                        let sample_count = data_buf.len() / 3;
                        pcm_samples.reserve(sample_count);
                        for i in 0..sample_count {
                            let b0 = data_buf[i * 3] as u32;
                            let b1 = data_buf[i * 3 + 1] as u32;
                            let b2 = data_buf[i * 3 + 2] as u32;
                            let mut val = (b0 | (b1 << 8) | (b2 << 16)) as i32;
                            if val & 0x0080_0000 != 0 {
                                val |= !0x00FF_FFFF;
                            }
                            pcm_samples.push(val as f32 / 8388608.0);
                        }
                    }
                    (3, 32) => {
                        let sample_count = data_buf.len() / 4;
                        pcm_samples.reserve(sample_count);
                        for i in 0..sample_count {
                            let val = f32::from_le_bytes([
                                data_buf[i * 4],
                                data_buf[i * 4 + 1],
                                data_buf[i * 4 + 2],
                                data_buf[i * 4 + 3],
                            ]);
                            pcm_samples.push(val);
                        }
                    }
                    (1, 32) => {
                        let sample_count = data_buf.len() / 4;
                        pcm_samples.reserve(sample_count);
                        for i in 0..sample_count {
                            let val = i32::from_le_bytes([
                                data_buf[i * 4],
                                data_buf[i * 4 + 1],
                                data_buf[i * 4 + 2],
                                data_buf[i * 4 + 3],
                            ]);
                            pcm_samples.push(val as f32 / 2147483648.0);
                        }
                    }
                    (1, 8) => {
                        let sample_count = data_buf.len();
                        pcm_samples.reserve(sample_count);
                        for &b in &data_buf {
                            pcm_samples.push((b as f32 - 128.0) / 128.0);
                        }
                    }
                    _ => {
                        return Err(format!(
                            "unsupported WAV format: format tag {audio_format}, bits {bits_per_sample}"
                        ));
                    }
                }
            }
            _ => {
                reader
                    .seek(SeekFrom::Current(chunk_size as i64))
                    .map_err(|e| format!("seek error: {e}"))?;
            }
        }

        if chunk_size % 2 != 0 {
            let _ = reader.seek(SeekFrom::Current(1));
        }
    }

    Ok((pcm_samples, sample_rate, channels))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn round_trip_bwf_wav() {
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("test_stem.wav");

        let sample_rate = 48000;
        let channels = 2;
        let bits_per_sample = 24;
        let time_ref = 96000; // exactly 2.0 seconds at 48kHz
        let samples = vec![0.0f32; 48000 * 2]; // 1 second of stereo silence

        write_bwf_pcm(
            &file_path,
            &samples,
            sample_rate,
            channels,
            bits_per_sample,
            time_ref,
            "Guitar Stem",
            "Toast Wave",
        )
        .unwrap();

        let info = read_bwf_info(&file_path).unwrap();
        assert_eq!(info.sample_rate, 48000);
        assert_eq!(info.channels, 2);
        assert_eq!(info.bits_per_sample, 24);
        assert_eq!(info.time_reference, Some(96000));
        assert_eq!(info.start_seconds, Some(2.0));
        assert_eq!(info.total_samples, 48000);
        assert!((info.duration_seconds - 1.0).abs() < 1e-6);
        assert_eq!(info.description, "Guitar Stem");
        assert_eq!(info.originator, "Toast Wave");

        // Verify read_pcm_f32
        let (read_samples, read_sr, read_ch) = read_pcm_f32(&file_path).unwrap();
        assert_eq!(read_sr, 48000);
        assert_eq!(read_ch, 2);
        assert_eq!(read_samples.len(), samples.len());
    }
}
