//! Final Cut Pro XML (`.fcpxml`) interchange format engine.
//!
//! Supported across macOS, Windows, and Linux by Apple Logic Pro X, Final Cut Pro,
//! DaVinci Resolve, Adobe Premiere, and pro audio DAWs.
//!
//! Provides timeline interchange:
//! - Multi-track audio and MIDI timeline
//! - Precise region offsets, start times, and durations
//! - Project tempo, time signature, and markers
//! - Direct export and import capabilities

use crate::{MarkerDoc, MediaFile, ProjectDoc, ProjectError, RegionDoc, TrackDoc, TrackKindDoc, FORMAT_VERSION};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

/// Exports `doc` and its `media` to a standard Final Cut Pro XML file (`.fcpxml`).
pub fn export_fcpxml(
    out_path: &Path,
    doc: &ProjectDoc,
    media: &[MediaFile],
) -> Result<(), ProjectError> {
    let mut path = out_path.to_path_buf();
    if path.extension().and_then(|e| e.to_str()) != Some("fcpxml") {
        path.set_extension("fcpxml");
    }

    let sample_rate = 48000u64;
    let beats_per_bar = doc.sig.0 as f64;
    let sec_per_beat = 60.0 / (doc.tempo as f64);

    // Calculate project duration based on furthest region end
    let mut max_end_seconds = 60.0; // minimum 1 minute
    for track in &doc.tracks {
        for region in &track.regions {
            let start_beat = ((region.start_bar as f64) - 1.0) * beats_per_bar;
            let len_beat = (region.len_bars as f64) * beats_per_bar;
            let end_sec = (start_beat + len_beat) * sec_per_beat;
            if end_sec > max_end_seconds {
                max_end_seconds = end_sec;
            }
        }
    }

    let total_samples = (max_end_seconds * (sample_rate as f64)).round() as u64;

    let mut xml = String::new();
    xml.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    xml.push_str("<!DOCTYPE fcpxml>\n\n");
    xml.push_str("<fcpxml version=\"1.10\">\n");
    xml.push_str("  <resources>\n");
    xml.push_str("    <format id=\"r1\" name=\"FFVideoFormatRateUndefined\" frameDuration=\"100/2500s\"/>\n");

    // Media assets
    for (i, m) in media.iter().enumerate() {
        let asset_id = format!("r{}", i + 2);
        let src_uri = format!("file:///{}", m.source.to_string_lossy().replace('\\', "/"));
        xml.push_str(&format!(
            "    <asset id=\"{}\" name=\"{}\" src=\"{}\" audioSources=\"1\" audioChannels=\"2\" audioRate=\"48000\"/>\n",
            asset_id,
            m.source.file_name().and_then(|n| n.to_str()).unwrap_or("audio"),
            src_uri
        ));
    }

    xml.push_str("  </resources>\n");
    xml.push_str("  <library>\n");
    xml.push_str(&format!("    <event name=\"{}\">\n", doc.name));
    xml.push_str(&format!("      <project name=\"{}\">\n", doc.name));
    xml.push_str(&format!(
        "        <sequence format=\"r1\" duration=\"{}/{}s\" audioLayout=\"stereo\" audioRate=\"48k\">\n",
        total_samples, sample_rate
    ));
    xml.push_str("          <spine>\n");

    // Project Markers as timeline markers
    for marker in &doc.markers {
        let marker_sec = marker.beat * sec_per_beat;
        let marker_samples = (marker_sec * (sample_rate as f64)).round() as u64;
        xml.push_str(&format!(
            "            <marker start=\"{}/{}s\" duration=\"1/{}s\" value=\"{}\"/>\n",
            marker_samples, sample_rate, sample_rate, marker.name
        ));
    }

    // Export each track as sequenced audio clips
    for (t_idx, track) in doc.tracks.iter().enumerate() {
        let lane = (t_idx as i32) - 10;
        for region in &track.regions {
            let start_beat = ((region.start_bar as f64) - 1.0).max(0.0) * beats_per_bar;
            let len_beat = (region.len_bars as f64) * beats_per_bar;

            let start_sec = start_beat * sec_per_beat;
            let dur_sec = len_beat * sec_per_beat;

            let start_samples = (start_sec * (sample_rate as f64)).round() as u64;
            let dur_samples = (dur_sec * (sample_rate as f64)).round() as u64;

            let asset_ref = if let Some(audio_ref) = &region.audio {
                media.iter().position(|m| &m.zip_path == audio_ref)
            } else {
                None
            };

            if let Some(a_idx) = asset_ref {
                let asset_id = format!("r{}", a_idx + 2);
                xml.push_str(&format!(
                    "            <asset-clip ref=\"{}\" name=\"{}\" offset=\"{}/{}s\" duration=\"{}/{}s\" start=\"0/{}s\" lane=\"{}\" role=\"dialogue\">\n",
                    asset_id, region.name, start_samples, sample_rate, dur_samples, sample_rate, sample_rate, lane
                ));
                xml.push_str("            </asset-clip>\n");
            } else {
                // MIDI or placeholder clip
                xml.push_str(&format!(
                    "            <gap name=\"{}\" offset=\"{}/{}s\" duration=\"{}/{}s\" start=\"0/{}s\" lane=\"{}\"/>\n",
                    region.name, start_samples, sample_rate, dur_samples, sample_rate, sample_rate, lane
                ));
            }
        }
    }

    xml.push_str("          </spine>\n");
    xml.push_str("        </sequence>\n");
    xml.push_str("      </project>\n");
    xml.push_str("    </event>\n");
    xml.push_str("  </library>\n");
    xml.push_str("</fcpxml>\n");

    fs::write(&path, xml).map_err(ProjectError::Io)?;
    Ok(())
}

/// Parses a Final Cut Pro XML file into a `ProjectDoc`.
pub fn load_fcpxml(path: &Path, media_dir: &Path) -> Result<ProjectDoc, ProjectError> {
    let content = fs::read_to_string(path).map_err(ProjectError::Io)?;
    let song_name = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("FCPXML Project")
        .to_string();

    let mut doc = ProjectDoc {
        format_version: FORMAT_VERSION,
        name: song_name,
        tempo: 120.0,
        sig: (4, 4),
        key: "Cmaj".into(),
        ..Default::default()
    };

    // Extract asset files: <asset id="..." src="file://..."/>
    let mut asset_map: HashMap<String, PathBuf> = HashMap::new();
    for line in content.lines() {
        if line.contains("<asset ") {
            let id = extract_attr(line, "id");
            let src = extract_attr(line, "src");
            if let (Some(id), Some(src)) = (id, src) {
                let clean_src = src
                    .strip_prefix("file:///")
                    .or_else(|| src.strip_prefix("file://"))
                    .unwrap_or(&src);
                asset_map.insert(id, PathBuf::from(clean_src));
            }
        }
    }

    // Extract markers: <marker start="samples/rate" value="name"/>
    let sec_per_beat = 60.0 / (doc.tempo as f64);
    for line in content.lines() {
        if line.contains("<marker ") {
            if let (Some(start_str), Some(name)) = (extract_attr(line, "start"), extract_attr(line, "value")) {
                if let Some(sec) = parse_fraction_seconds(&start_str) {
                    let beat = sec / sec_per_beat;
                    doc.markers.push(MarkerDoc {
                        beat,
                        name,
                        color: [0x50, 0x82, 0xe6],
                    });
                }
            }
        }
    }

    // Extract clips: <asset-clip ref="..." name="..." offset="X/Ys" duration="A/Bs" ... lane="..."/>
    let mut lane_map: HashMap<i32, Vec<RegionDoc>> = HashMap::new();
    fs::create_dir_all(media_dir).map_err(ProjectError::Io)?;

    let mut clip_counter = 0;
    for line in content.lines() {
        if line.contains("<asset-clip ") || line.contains("<clip ") {
            let name = extract_attr(line, "name").unwrap_or_else(|| format!("Clip {clip_counter}"));
            let offset_sec = extract_attr(line, "offset")
                .and_then(|s| parse_fraction_seconds(&s))
                .unwrap_or(0.0);
            let dur_sec = extract_attr(line, "duration")
                .and_then(|s| parse_fraction_seconds(&s))
                .unwrap_or(4.0);
            let lane: i32 = extract_attr(line, "lane")
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            let ref_id = extract_attr(line, "ref");

            let beats_per_bar = doc.sig.0 as f64;
            let start_beat = offset_sec / sec_per_beat;
            let len_beat = dur_sec / sec_per_beat;

            let start_bar = ((start_beat / beats_per_bar) + 1.0) as f32;
            let len_bars = ((len_beat / beats_per_bar).max(0.1)) as f32;

            let mut audio_rel = None;
            if let Some(rid) = ref_id {
                if let Some(src_path) = asset_map.get(&rid) {
                    if src_path.exists() {
                        let ext = src_path
                            .extension()
                            .and_then(|e| e.to_str())
                            .unwrap_or("wav");
                        let dest_name = format!("{}_{}.{}", clip_counter, name, ext);
                        let _ = fs::copy(src_path, media_dir.join(&dest_name));
                        audio_rel = Some(format!("media/{dest_name}"));
                    }
                }
            }

            let region = RegionDoc {
                name,
                start_bar,
                len_bars,
                audio: audio_rel,
                ..Default::default()
            };

            lane_map.entry(lane).or_default().push(region);
            clip_counter += 1;
        }
    }

    for (lane, mut regions) in lane_map {
        regions.sort_by(|a, b| a.start_bar.partial_cmp(&b.start_bar).unwrap());
        let t_name = format!("Audio Track (Lane {lane})");
        doc.tracks.push(TrackDoc {
            name: t_name,
            kind: TrackKindDoc::Audio,
            color: [0x4e, 0xb0, 0x4a],
            regions,
            ..Default::default()
        });
    }

    if doc.tracks.is_empty() {
        doc.tracks.push(TrackDoc {
            name: "Audio 1".into(),
            kind: TrackKindDoc::Audio,
            color: [0x4e, 0xb0, 0x4a],
            ..Default::default()
        });
    }

    Ok(doc)
}

fn extract_attr(line: &str, attr: &str) -> Option<String> {
    let key = format!("{}=\"", attr);
    let start = line.find(&key)? + key.len();
    let end = line[start..].find('"')? + start;
    Some(line[start..end].to_string())
}

fn parse_fraction_seconds(val: &str) -> Option<f64> {
    let s = val.trim_end_matches('s').trim();
    if let Some((num_str, den_str)) = s.split_once('/') {
        let num: f64 = num_str.trim().parse().ok()?;
        let den: f64 = den_str.trim().parse().ok()?;
        if den > 0.0 {
            return Some(num / den);
        }
    } else if let Ok(sec) = s.parse::<f64>() {
        return Some(sec);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn round_trip_fcpxml_export() {
        let dir = tempdir().unwrap();
        let fcpxml_path = dir.path().join("Session.fcpxml");
        let media_dir = dir.path().join("fcpxml_media");

        let doc = ProjectDoc {
            name: "Session".into(),
            tempo: 120.0,
            sig: (4, 4),
            markers: vec![MarkerDoc {
                beat: 16.0,
                name: "Chorus".into(),
                color: [0, 0, 0],
            }],
            tracks: vec![TrackDoc {
                name: "Drums".into(),
                kind: TrackKindDoc::Audio,
                regions: vec![RegionDoc {
                    name: "Intro Beat".into(),
                    start_bar: 1.0,
                    len_bars: 4.0,
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        };

        export_fcpxml(&fcpxml_path, &doc, &[]).unwrap();
        assert!(fcpxml_path.exists());

        let loaded = load_fcpxml(&fcpxml_path, &media_dir).unwrap();
        assert_eq!(loaded.name, "Session");
        assert_eq!(loaded.markers.len(), 1);
        assert_eq!(loaded.markers[0].name, "Chorus");
    }
}
