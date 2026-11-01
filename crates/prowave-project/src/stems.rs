//! Multi-track Broadcast Wave Format (BWF) stems and session report export engine.
//!
//! Generates stems for cross-platform DAW interchange (Pro Tools, Ableton Live, FL Studio,
//! Studio One, Reaper, Cubase, Logic Pro, Bitwig):
//! - Mode A: Consolidated Stems (zero-aligned from Bar 1 Beat 1 sample 0 for 1-click import)
//! - Mode B: Timestamped BWF Stems (with `bext` TimeReference 64-bit sample offsets)
//! - Standard MIDI Files (`.mid` Type 1 SMF) for instrument and drummer tracks
//! - `export_manifest.json` machine-readable interchange manifest
//! - `SESSION_INFO.txt` comprehensive session documentation sheet

use crate::bwf::{read_pcm_f32, write_bwf_copy, write_bwf_pcm};
use crate::{MediaFile, ProjectDoc, ProjectError};
use serde::{Deserialize, Serialize};
use std::fs::{self, File};
use std::io::Write;
use std::path::Path;

/// Export mode for audio stems.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StemExportMode {
    /// Zero-aligned: each track starts at sample 0 (Bar 1, Beat 1), padded with silence.
    /// Ideal for drag-and-drop into Pro Tools, Ableton, FL Studio, Reaper, Cubase.
    Consolidated,
    /// Preserves individual region cuts with sample-accurate BWF `bext` TimeReference stamps.
    Timestamped,
}

/// Metadata entry in `export_manifest.json`.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct StemManifestEntry {
    pub track_index: usize,
    pub track_name: String,
    pub file_name: String,
    pub kind: String,
    pub fader_db: f32,
    pub pan: f32,
    pub muted: bool,
    pub solo: bool,
    pub plugins: Vec<String>,
    pub start_time_seconds: f64,
    pub time_reference: u64,
}

/// Marker entry in manifest.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct ManifestMarker {
    pub name: String,
    pub bar: f64,
    pub beat: f64,
    pub time_seconds: f64,
}

/// Root manifest schema.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct ExportManifest {
    pub format: String,
    pub project_name: String,
    pub tempo: f32,
    pub time_signature: String,
    pub key: String,
    pub sample_rate: u32,
    pub bit_depth: u16,
    pub mode: String,
    pub stems: Vec<StemManifestEntry>,
    pub markers: Vec<ManifestMarker>,
}

/// Exports project stems, MIDI files, manifest, and session info report to `out_dir`.
pub fn export_bwf_stems(
    doc: &ProjectDoc,
    media: &[MediaFile],
    out_dir: &Path,
    mode: StemExportMode,
) -> Result<ExportManifest, ProjectError> {
    fs::create_dir_all(out_dir)?;
    let stems_dir = out_dir.join("Stems");
    let midi_dir = out_dir.join("MIDI");
    fs::create_dir_all(&stems_dir)?;
    fs::create_dir_all(&midi_dir)?;

    let sample_rate = 48000u32;
    let bits_per_sample = 24u16;
    let beats_per_bar = doc.sig.0 as f64;
    let sec_per_beat = 60.0 / (doc.tempo as f64);

    // Compute project duration
    let mut max_end_seconds = 10.0;
    for track in &doc.tracks {
        for region in &track.regions {
            let start_beat = ((region.start_bar as f64) - 1.0).max(0.0) * beats_per_bar;
            let len_beat = (region.len_bars as f64) * beats_per_bar;
            let end_sec = (start_beat + len_beat) * sec_per_beat;
            if end_sec > max_end_seconds {
                max_end_seconds = end_sec;
            }
        }
    }
    // Add 2 seconds ring-out
    let total_duration_seconds = max_end_seconds + 2.0;
    let total_samples = (total_duration_seconds * sample_rate as f64).round() as usize;

    let mut manifest_entries: Vec<StemManifestEntry> = Vec::new();

    // Export Audio Stems
    for (t_idx, track) in doc.tracks.iter().enumerate() {
        let clean_track_name = if track.name.trim().is_empty() {
            format!("Track_{}", t_idx + 1)
        } else {
            track
                .name
                .chars()
                .map(|c| if c.is_alphanumeric() || c == '_' || c == '-' { c } else { '_' })
                .collect()
        };

        // Check if track has audio regions
        let audio_regions: Vec<_> = track
            .regions
            .iter()
            .filter_map(|r| r.audio.as_ref().map(|a| (r, a)))
            .collect();

        if !audio_regions.is_empty() {
            match mode {
                StemExportMode::Consolidated => {
                    let file_name = format!("{:02}_{}.wav", t_idx + 1, clean_track_name);
                    let out_path = stems_dir.join(&file_name);

                    // 2-channel stereo buffer
                    let channels = 2u16;
                    let mut track_buffer = vec![0.0f32; total_samples * channels as usize];

                    for (region, audio_ref) in &audio_regions {
                        let region_start_beat =
                            ((region.start_bar as f64) - 1.0).max(0.0) * beats_per_bar;
                        let region_start_sec = region_start_beat * sec_per_beat;
                        let start_frame =
                            (region_start_sec * sample_rate as f64).round() as usize;

                        // Find media source
                        if let Some(m) = media.iter().find(|m| &m.zip_path == *audio_ref) {
                            if let Ok((samples, _src_sr, src_ch)) = read_pcm_f32(&m.source) {
                                let frames = if src_ch > 0 {
                                    samples.len() / (src_ch as usize)
                                } else {
                                    0
                                };
                                for frame in 0..frames {
                                    let target_frame = start_frame + frame;
                                    if target_frame >= total_samples {
                                        break;
                                    }
                                    if src_ch == 1 {
                                        let s = samples[frame];
                                        track_buffer[target_frame * 2] += s;
                                        track_buffer[target_frame * 2 + 1] += s;
                                    } else {
                                        let s_left = samples[frame * src_ch as usize];
                                        let s_right = samples[frame * src_ch as usize + 1];
                                        track_buffer[target_frame * 2] += s_left;
                                        track_buffer[target_frame * 2 + 1] += s_right;
                                    }
                                }
                            }
                        }
                    }

                    // Write consolidated 24-bit BWF file (sample 0 time reference)
                    let _ = write_bwf_pcm(
                        &out_path,
                        &track_buffer,
                        sample_rate,
                        channels,
                        bits_per_sample,
                        0,
                        &format!("Stem: {}", track.name),
                        "Toast Wave DAW",
                    );

                    manifest_entries.push(StemManifestEntry {
                        track_index: t_idx,
                        track_name: track.name.clone(),
                        file_name,
                        kind: format!("{:?}", track.kind),
                        fader_db: track.volume_db,
                        pan: track.pan,
                        muted: track.mute,
                        solo: track.solo,
                        plugins: track.inserts.clone(),
                        start_time_seconds: 0.0,
                        time_reference: 0,
                    });
                }
                StemExportMode::Timestamped => {
                    for (r_idx, (region, audio_ref)) in audio_regions.iter().enumerate() {
                        let region_start_beat =
                            ((region.start_bar as f64) - 1.0).max(0.0) * beats_per_bar;
                        let region_start_sec = region_start_beat * sec_per_beat;
                        let time_reference =
                            (region_start_sec * sample_rate as f64).round() as u64;

                        let file_name = format!(
                            "{:02}_{}_{:02}.wav",
                            t_idx + 1,
                            clean_track_name,
                            r_idx + 1
                        );
                        let out_path = stems_dir.join(&file_name);

                        if let Some(m) = media.iter().find(|m| &m.zip_path == *audio_ref) {
                            let _ = write_bwf_copy(
                                &m.source,
                                &out_path,
                                time_reference,
                                &format!("Region: {}", region.name),
                                "Toast Wave DAW",
                            );
                        }

                        manifest_entries.push(StemManifestEntry {
                            track_index: t_idx,
                            track_name: track.name.clone(),
                            file_name,
                            kind: format!("{:?}", track.kind),
                            fader_db: track.volume_db,
                            pan: track.pan,
                            muted: track.mute,
                            solo: track.solo,
                            plugins: track.inserts.clone(),
                            start_time_seconds: region_start_sec,
                            time_reference,
                        });
                    }
                }
            }
        }

        // Export MIDI if track has MIDI notes
        let has_notes = track.regions.iter().any(|r| !r.notes.is_empty());
        if has_notes {
            let midi_filename = format!("{:02}_{}.mid", t_idx + 1, clean_track_name);
            let midi_path = midi_dir.join(&midi_filename);
            let mut all_track_notes: Vec<(f64, f64, u8, u8)> = Vec::new();
            for r in &track.regions {
                let r_start_beat = ((r.start_bar as f64) - 1.0).max(0.0) * beats_per_bar;
                for n in &r.notes {
                    let note_beat = r_start_beat + (n.start as f64);
                    all_track_notes.push((note_beat, n.len as f64, n.pitch, n.vel));
                }
            }
            let _ = write_simple_smf(
                &midi_path,
                &track.name,
                doc.tempo,
                doc.sig,
                &all_track_notes,
            );
        }
    }

    // Manifest Markers
    let markers: Vec<ManifestMarker> = doc
        .markers
        .iter()
        .map(|m| {
            let time_sec = m.beat * sec_per_beat;
            let bar = (m.beat / beats_per_bar) + 1.0;
            ManifestMarker {
                name: m.name.clone(),
                bar,
                beat: m.beat,
                time_seconds: time_sec,
            }
        })
        .collect();

    let manifest = ExportManifest {
        format: "Toast Wave Multi-Platform Export v1.0".into(),
        project_name: doc.name.clone(),
        tempo: doc.tempo,
        time_signature: format!("{}/{}", doc.sig.0, doc.sig.1),
        key: doc.key.clone(),
        sample_rate,
        bit_depth: bits_per_sample,
        mode: match mode {
            StemExportMode::Consolidated => "Consolidated (Zero-Aligned)".into(),
            StemExportMode::Timestamped => "Timestamped (BWF TimeReference)".into(),
        },
        stems: manifest_entries,
        markers,
    };

    // Write export_manifest.json
    let json_bytes = serde_json::to_vec_pretty(&manifest)
        .map_err(|e| ProjectError::Json(e.to_string()))?;
    fs::write(out_dir.join("export_manifest.json"), json_bytes)?;

    // Write SESSION_INFO.txt
    let session_info = generate_session_info(doc, &manifest);
    fs::write(out_dir.join("SESSION_INFO.txt"), session_info.as_bytes())?;

    Ok(manifest)
}

/// Generates a formatted session report text file.
fn generate_session_info(doc: &ProjectDoc, manifest: &ExportManifest) -> String {
    let mut s = String::new();
    s.push_str("================================================================================\n");
    s.push_str("                    TOAST WAVE - SESSION INTERCHANGE REPORT                     \n");
    s.push_str("================================================================================\n\n");

    s.push_str(&format!("Project Name:     {}\n", doc.name));
    s.push_str("DAW Origin:       Toast Wave DAW (Apple Logic Pro X Alternative)\n");
    s.push_str(&format!("Export Mode:      {}\n", manifest.mode));
    s.push_str(&format!("Sample Rate:      {} Hz\n", manifest.sample_rate));
    s.push_str(&format!("Bit Depth:        {}-bit Linear PCM\n", manifest.bit_depth));
    s.push_str(&format!("Tempo:            {:.3} BPM\n", doc.tempo));
    s.push_str(&format!("Time Signature:   {}/{}\n", doc.sig.0, doc.sig.1));
    s.push_str(&format!("Project Key:      {}\n", doc.key));
    s.push_str(&format!("Timecode Rate:    {} fps\n\n", doc.smpte_rate));

    s.push_str("--------------------------------------------------------------------------------\n");
    s.push_str("PROJECT MARKERS\n");
    s.push_str("--------------------------------------------------------------------------------\n");
    if manifest.markers.is_empty() {
        s.push_str("(No markers defined in project)\n\n");
    } else {
        s.push_str(&format!("{:<4} {:<10} {:<10} {:<12} {}\n", "#", "Bar", "Beat", "Time", "Marker Name"));
        s.push_str("--------------------------------------------------------------------------------\n");
        for (i, m) in manifest.markers.iter().enumerate() {
            let min = (m.time_seconds / 60.0).floor() as u32;
            let sec = m.time_seconds % 60.0;
            let time_str = format!("{:02}:{:06.3}", min, sec);
            s.push_str(&format!(
                "{:<4} {:<10.2} {:<10.2} {:<12} {}\n",
                i + 1,
                m.bar,
                m.beat,
                time_str,
                m.name
            ));
        }
        s.push('\n');
    }

    s.push_str("--------------------------------------------------------------------------------\n");
    s.push_str("TRACK & STEM LIST\n");
    s.push_str("--------------------------------------------------------------------------------\n");
    s.push_str(&format!("{:<4} {:<24} {:<12} {:<8} {:<8} {}\n", "#", "Track Name", "Type", "Vol", "Pan", "Plugins"));
    s.push_str("--------------------------------------------------------------------------------\n");
    for (i, track) in doc.tracks.iter().enumerate() {
        let plugins = if track.inserts.is_empty() {
            "-".to_string()
        } else {
            track.inserts.join(", ")
        };
        let pan_str = if track.pan.abs() < 0.01 {
            "Center".to_string()
        } else if track.pan < 0.0 {
            format!("{:.0}L", -track.pan * 100.0)
        } else {
            format!("{:.0}R", track.pan * 100.0)
        };
        s.push_str(&format!(
            "{:<4} {:<24} {:<12?} {:<8.2} {:<8} {}\n",
            i + 1,
            track.name,
            track.kind,
            track.volume_db,
            pan_str,
            plugins
        ));
    }
    s.push('\n');

    s.push_str("--------------------------------------------------------------------------------\n");
    s.push_str("CROSS-PLATFORM DAW IMPORT INSTRUCTIONS\n");
    s.push_str("--------------------------------------------------------------------------------\n");
    s.push_str("1. Pro Tools, Ableton Live, FL Studio, Reaper, Cubase, Studio One:\n");
    s.push_str("   • Create a new empty session at matching BPM (");
    s.push_str(&format!("{:.1}", doc.tempo));
    s.push_str(") and 48 kHz / 24-bit.\n");
    s.push_str("   • Drag all WAV files from the 'Stems/' folder into the arrangement at Bar 1 Beat 1 (0:00:00).\n");
    s.push_str("   • All consolidated stems are zero-aligned and lock sample-accurately into sync.\n\n");
    s.push_str("2. Apple Logic Pro X:\n");
    s.push_str("   • Open the generated .logicx package or import stems via File > Import > Audio Files.\n");
    s.push_str("   • All stems contain BWF bext chunks with embedded TimeReference samples.\n\n");
    s.push_str("3. Final Cut Pro / Premiere / DaVinci Resolve:\n");
    s.push_str("   • File > Import Final Cut Pro XML (.fcpxml) to load sequence with volume and markers.\n");
    s.push_str("================================================================================\n");

    s
}

/// Simple Standard MIDI File (SMF Type 1) writer.
fn write_simple_smf(
    path: &Path,
    track_name: &str,
    tempo: f32,
    sig: (u8, u8),
    notes: &[(f64, f64, u8, u8)], // (start_beat, len_beat, pitch, vel)
) -> Result<(), std::io::Error> {
    let ppq = 480u16;

    // Conductor Track (Tempo & Sig)
    let mut conductor_events: Vec<u8> = Vec::new();
    // Time Sig: 00 FF 58 04 nn dd cc bb
    conductor_events.extend_from_slice(&[0x00, 0xFF, 0x58, 0x04]);
    let den_pow = match sig.1 {
        1 => 0,
        2 => 1,
        4 => 2,
        8 => 3,
        16 => 4,
        32 => 5,
        _ => 2,
    };
    conductor_events.push(sig.0);
    conductor_events.push(den_pow);
    conductor_events.push(24);
    conductor_events.push(8);

    // Set Tempo: 00 FF 51 03 tt tt tt
    let us_per_quarter = (60_000_000.0 / (tempo as f64)).round() as u32;
    conductor_events.extend_from_slice(&[0x00, 0xFF, 0x51, 0x03]);
    conductor_events.push(((us_per_quarter >> 16) & 0xFF) as u8);
    conductor_events.push(((us_per_quarter >> 8) & 0xFF) as u8);
    conductor_events.push((us_per_quarter & 0xFF) as u8);

    // End of track: 00 FF 2F 00
    conductor_events.extend_from_slice(&[0x00, 0xFF, 0x2F, 0x00]);

    // Note Track
    let mut note_events: Vec<u8> = Vec::new();

    // Track Name: 00 FF 03 len bytes
    let name_bytes = track_name.as_bytes();
    note_events.extend_from_slice(&[0x00, 0xFF, 0x03]);
    write_vlq(&mut note_events, name_bytes.len() as u64);
    note_events.extend_from_slice(name_bytes);

    // Sort notes and generate on/off events
    // (tick, is_off, pitch, vel)
    let mut raw_events: Vec<(u64, bool, u8, u8)> = Vec::new();
    for &(start_b, len_b, pitch, vel) in notes {
        let on_tick = (start_b * ppq as f64).round() as u64;
        let off_tick = ((start_b + len_b) * ppq as f64).round() as u64;
        raw_events.push((on_tick, false, pitch, vel));
        raw_events.push((off_tick, true, pitch, 0));
    }
    raw_events.sort_by(|a, b| {
        if a.0 != b.0 {
            a.0.cmp(&b.0)
        } else {
            // Off before On at same tick
            b.1.cmp(&a.1)
        }
    });

    let mut cur_tick = 0u64;
    for (tick, is_off, pitch, vel) in raw_events {
        let delta = tick.saturating_sub(cur_tick);
        write_vlq(&mut note_events, delta);
        if is_off {
            note_events.push(0x80);
            note_events.push(pitch);
            note_events.push(0x00);
        } else {
            note_events.push(0x90);
            note_events.push(pitch);
            note_events.push(vel);
        }
        cur_tick = tick;
    }

    // End of track
    note_events.extend_from_slice(&[0x00, 0xFF, 0x2F, 0x00]);

    // Build SMF file
    let mut file = File::create(path)?;
    // MThd
    file.write_all(b"MThd")?;
    file.write_all(&6u32.to_be_bytes())?;
    file.write_all(&1u16.to_be_bytes())?; // Format 1
    file.write_all(&2u16.to_be_bytes())?; // 2 tracks (conductor + note track)
    file.write_all(&ppq.to_be_bytes())?;

    // MTrk 1 (Conductor)
    file.write_all(b"MTrk")?;
    file.write_all(&(conductor_events.len() as u32).to_be_bytes())?;
    file.write_all(&conductor_events)?;

    // MTrk 2 (Notes)
    file.write_all(b"MTrk")?;
    file.write_all(&(note_events.len() as u32).to_be_bytes())?;
    file.write_all(&note_events)?;

    Ok(())
}

fn write_vlq(out: &mut Vec<u8>, mut val: u64) {
    let mut buf = [0u8; 10];
    let mut i = 0;
    buf[i] = (val & 0x7F) as u8;
    val >>= 7;
    while val > 0 {
        i += 1;
        buf[i] = ((val & 0x7F) | 0x80) as u8;
        val >>= 7;
    }
    while i > 0 {
        out.push(buf[i]);
        i -= 1;
    }
    out.push(buf[0]);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{MarkerDoc, NoteDoc, RegionDoc, TrackDoc};
    use tempfile::tempdir;

    #[test]
    fn test_export_bwf_stems_round_trip() {
        use crate::TrackKindDoc;
        let dir = tempdir().unwrap();
        let audio_src = dir.path().join("source_guitar.wav");

        // Create dummy source WAV
        let sample_rate = 48000;
        let pcm = vec![0.0f32; 48000 * 2]; // 1 second
        write_bwf_pcm(
            &audio_src,
            &pcm,
            sample_rate,
            2,
            24,
            0,
            "Guitar Audio",
            "Toast Wave",
        )
        .unwrap();

        let doc = ProjectDoc {
            name: "TestSong".into(),
            tempo: 120.0,
            sig: (4, 4),
            key: "Gmaj".into(),
            tracks: vec![
                TrackDoc {
                    name: "Guitars".into(),
                    kind: TrackKindDoc::Audio,
                    volume_db: 0.0,
                    pan: -0.2,
                    inserts: vec!["Channel EQ".into(), "Compressor".into()],
                    regions: vec![RegionDoc {
                        name: "Guitar Riff".into(),
                        start_bar: 1.0,
                        len_bars: 2.0,
                        audio: Some("media/0_source_guitar.wav".into()),
                        ..Default::default()
                    }],
                    ..Default::default()
                },
                TrackDoc {
                    name: "Synth Lead".into(),
                    kind: TrackKindDoc::Instrument,
                    volume_db: -2.0,
                    pan: 0.2,
                    regions: vec![RegionDoc {
                        name: "Lead Melody".into(),
                        start_bar: 2.0,
                        len_bars: 2.0,
                        notes: vec![
                            NoteDoc {
                                start: 0.0,
                                len: 1.0,
                                pitch: 60,
                                vel: 100,
                            },
                            NoteDoc {
                                start: 1.0,
                                len: 1.0,
                                pitch: 62,
                                vel: 90,
                            },
                        ],
                        ..Default::default()
                    }],
                    ..Default::default()
                },
            ],
            markers: vec![
                MarkerDoc {
                    beat: 0.0,
                    name: "Intro".into(),
                    color: [255, 0, 0],
                },
                MarkerDoc {
                    beat: 16.0,
                    name: "Chorus".into(),
                    color: [0, 255, 0],
                },
            ],
            ..Default::default()
        };

        let media = vec![MediaFile {
            zip_path: "media/0_source_guitar.wav".into(),
            source: audio_src,
        }];

        let export_dir = dir.path().join("Export");
        let manifest = export_bwf_stems(
            &doc,
            &media,
            &export_dir,
            StemExportMode::Consolidated,
        )
        .unwrap();

        assert_eq!(manifest.project_name, "TestSong");
        assert_eq!(manifest.tempo, 120.0);
        assert_eq!(manifest.stems.len(), 1);
        assert_eq!(manifest.markers.len(), 2);

        // Verify output files exist
        assert!(export_dir.join("export_manifest.json").exists());
        assert!(export_dir.join("SESSION_INFO.txt").exists());
        assert!(export_dir.join("Stems/01_Guitars.wav").exists());
        assert!(export_dir.join("MIDI/02_Synth_Lead.mid").exists());

        // Verify SESSION_INFO.txt content
        let report = fs::read_to_string(export_dir.join("SESSION_INFO.txt")).unwrap();
        assert!(report.contains("TestSong"));
        assert!(report.contains("120.000 BPM"));
        assert!(report.contains("Guitars"));
        assert!(report.contains("Synth Lead"));
        assert!(report.contains("Intro"));
        assert!(report.contains("Chorus"));
    }
}
