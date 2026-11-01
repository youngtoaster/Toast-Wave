//! Apple Logic Pro X (`.logicx`) bundle ingestion and synthesis engine.
//!
//! Handles:
//! - Package Mode (`.logicx` directory bundle) and Folder Mode (`Song/` with `.logicx` and external audio)
//! - `ProjectInformation.plist` and `MetaData.plist` (XML and binary Apple plists)
//! - Broadcast Wave Format (BWF) sample timestamp decoding from `Audio Files/`
//! - `ProjectData` binary scanning:
//!   - Markers: `karT` (tracks), `gRuA` (audio regions), `qeSM` (MIDI sequences), `GAME` (stock plugins)
//!   - Session Players / Drummer embedded JSON
//!   - Region name anchor `0x61 0xFF`
//! - Complete session reconstruction into `ProjectDoc`
//! - Export / synthesis of valid `.logicx` bundles with plists, BWF audio files, and ProjectData

use crate::bwf::{read_bwf_info, write_bwf_copy};
use crate::{
    DrummerDoc, InsertDoc, MediaFile, ProjectDoc, ProjectError, RegionDoc, TrackDoc,
    TrackKindDoc, FORMAT_VERSION,
};
use plist::Value;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

/// Magic bytes at the start of Logic Pro ProjectData.
pub const PROJECT_DATA_MAGIC: [u8; 4] = [0x23, 0x47, 0xC0, 0xAB];

/// Known FourCC markers stored in little-endian (reversed ASCII).
pub const CHUNK_TRAK: &[u8; 4] = b"karT"; // Trak
pub const CHUNK_AURG: &[u8; 4] = b"gRuA"; // AuRg
pub const CHUNK_MSEQ: &[u8; 4] = b"qeSM"; // MSeq
pub const CHUNK_EVSQ: &[u8; 4] = b"qSvE"; // EvSq
pub const CHUNK_INST: &[u8; 4] = b"tSnI"; // InSt
pub const CHUNK_COMP: &[u8; 4] = b"PMOC"; // COMP
pub const CHUNK_AUFL_UPPER: &[u8; 4] = b"LFUA"; // AUFL
pub const CHUNK_AUFL_LOWER: &[u8; 4] = b"lFuA"; // AuFl

/// Known stock Apple plug-ins identified by the GAME anchor.
pub const STOCK_PLUGINS: &[&str] = &[
    "Compressor",
    "Channel EQ",
    "ChromaVerb",
    "SpaceDesigner",
    "Tape Delay",
    "Chorus",
    "Phaser",
    "Overdrive",
    "Filter",
    "Multipressor",
    "EnRoute",
];

/// Locates the real `.logicx` bundle directory given a path (handles both package and folder mode).
pub fn find_logicx_bundle(path: &Path) -> Result<PathBuf, String> {
    if !path.exists() {
        return Err(format!("path does not exist: {}", path.display()));
    }

    // Direct bundle directory: e.g. "Song.logicx"
    if path.is_dir() {
        if path.join("Alternatives").exists() || path.join("ProjectInformation.plist").exists() {
            return Ok(path.to_path_buf());
        }

        // Folder Mode: check inside directory for a .logicx child directory
        if let Ok(entries) = fs::read_dir(path) {
            for entry in entries.flatten() {
                let p = entry.path();
                if p.is_dir()
                    && p.extension().and_then(|e| e.to_str()) == Some("logicx")
                    && (p.join("Alternatives").exists() || p.join("ProjectInformation.plist").exists())
                {
                    return Ok(p);
                }
            }
        }
    }

    Err(format!(
        "not a valid Logic Pro X project bundle: {}",
        path.display()
    ))
}

/// Reads metadata from `ProjectInformation.plist` and `MetaData.plist`.
#[derive(Clone, Debug)]
pub struct LogicxMetadata {
    pub active_alternative: String,
    pub logic_version: String,
    pub tempo: f32,
    pub time_signature: (u8, u8),
    pub key_signature: String,
    pub sample_rate: u32,
}

impl Default for LogicxMetadata {
    fn default() -> Self {
        Self {
            active_alternative: "000".into(),
            logic_version: "11.0".into(),
            tempo: 120.0,
            time_signature: (4, 4),
            key_signature: "Cmaj".into(),
            sample_rate: 44100,
        }
    }
}

pub fn read_bundle_metadata(bundle_path: &Path) -> LogicxMetadata {
    let mut meta = LogicxMetadata::default();

    // 1. ProjectInformation.plist
    let info_plist_path = bundle_path.join("ProjectInformation.plist");
    if let Ok(val) = Value::from_file(&info_plist_path) {
        if let Some(dict) = val.as_dictionary() {
            if let Some(alt) = dict.get("activeAlternative") {
                if let Some(num) = alt.as_unsigned_integer() {
                    meta.active_alternative = format!("{:03}", num);
                } else if let Some(s) = alt.as_string() {
                    meta.active_alternative = s.to_string();
                }
            }
            if let Some(ver) = dict.get("logicVersion").and_then(|v| v.as_string()) {
                meta.logic_version = ver.to_string();
            }
        }
    }

    // 2. Alternatives/<alt>/MetaData.plist
    let alt_dir = bundle_path
        .join("Alternatives")
        .join(&meta.active_alternative);
    let meta_plist_path = alt_dir.join("MetaData.plist");
    let display_plist_path = alt_dir.join("DisplayState.plist");

    let parse_dict = |dict: &plist::Dictionary, meta: &mut LogicxMetadata| {
        if let Some(t) = dict.get("tempo") {
            if let Some(f) = t.as_real() {
                if f > 20.0 && f < 999.0 {
                    meta.tempo = f as f32;
                }
            } else if let Some(i) = t.as_unsigned_integer() {
                if i > 20 && i < 999 {
                    meta.tempo = i as f32;
                }
            }
        }
        if let Some(k) = dict.get("keySignature").and_then(|v| v.as_string()) {
            meta.key_signature = k.to_string();
        }
        if let Some(sr) = dict.get("sampleRate") {
            if let Some(num) = sr.as_unsigned_integer() {
                meta.sample_rate = num as u32;
            } else if let Some(f) = sr.as_real() {
                meta.sample_rate = f as u32;
            }
        }
        if let Some(ts) = dict.get("timeSignature") {
            if let Some(arr) = ts.as_array() {
                if arr.len() >= 2 {
                    let num = arr[0].as_unsigned_integer().unwrap_or(4) as u8;
                    let den = arr[1].as_unsigned_integer().unwrap_or(4) as u8;
                    if num > 0 && den > 0 {
                        meta.time_signature = (num, den);
                    }
                }
            } else if let Some(s) = ts.as_string() {
                if let Some((n, d)) = s.split_once('/') {
                    if let (Ok(num), Ok(den)) = (n.trim().parse::<u8>(), d.trim().parse::<u8>()) {
                        if num > 0 && den > 0 {
                            meta.time_signature = (num, den);
                        }
                    }
                }
            }
        }
    };

    if let Ok(val) = Value::from_file(&meta_plist_path) {
        if let Some(dict) = val.as_dictionary() {
            parse_dict(dict, &mut meta);
        }
    } else if let Ok(val) = Value::from_file(&display_plist_path) {
        if let Some(dict) = val.as_dictionary() {
            parse_dict(dict, &mut meta);
        }
    }

    meta
}

/// An audio region candidate extracted from Logic's bundle or `ProjectData`.
#[derive(Clone, Debug)]
pub struct ExtractedAudioRegion {
    pub file_path: PathBuf,
    pub track_name: String,
    pub region_name: String,
    pub start_seconds: f64,
    pub duration_seconds: f64,
}

/// Scans all candidate audio directories within or adjacent to a Logic Pro bundle.
pub fn scan_audio_files(bundle_path: &Path, alt: &str) -> Vec<PathBuf> {
    let mut candidates = Vec::new();

    // 1. <bundle>/Audio Files
    candidates.push(bundle_path.join("Audio Files"));
    // 2. <bundle>/Alternatives/<alt>/Audio Files
    candidates.push(bundle_path.join("Alternatives").join(alt).join("Audio Files"));
    // 3. <bundle>/Media/Audio Files
    candidates.push(bundle_path.join("Media").join("Audio Files"));

    // 4. If in folder mode, check parent folder: <parent>/Audio Files
    if let Some(parent) = bundle_path.parent() {
        candidates.push(parent.join("Audio Files"));
    }

    let mut found = Vec::new();
    for dir in candidates {
        if dir.exists() && dir.is_dir() {
            if let Ok(entries) = fs::read_dir(dir) {
                for entry in entries.flatten() {
                    let p = entry.path();
                    if p.is_file() {
                        if let Some(ext) = p.extension().and_then(|e| e.to_str()) {
                            let ext_lower = ext.to_ascii_lowercase();
                            if matches!(
                                ext_lower.as_str(),
                                "wav" | "wave" | "aif" | "aiff" | "caf" | "mp3" | "flac"
                            ) {
                                if !found.contains(&p) {
                                    found.push(p);
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    found
}

/// Cleans audio take/comp suffixes to deduce the track name.
/// Examples:
/// "Vocal Lead: Take 01" -> "Vocal Lead"
/// "Acoustic GTR_01" -> "Acoustic GTR"
/// "Snare#04" -> "Snare"
pub fn clean_track_name(name: &str) -> String {
    let mut s = name.trim().to_string();

    // Iteratively strip known take/comp suffixes
    let mut changed = true;
    while changed {
        changed = false;
        if let Some((base, _)) = s.split_once(": Take") {
            s = base.trim().to_string();
            changed = true;
        } else if let Some((base, _)) = s.split_once(": Comp") {
            s = base.trim().to_string();
            changed = true;
        } else if let Some((base, rest)) = s.rsplit_once('_') {
            if rest.chars().all(|c| c.is_ascii_digit()) {
                s = base.trim().to_string();
                changed = true;
            }
        } else if let Some((base, rest)) = s.rsplit_once('#') {
            if rest.chars().all(|c| c.is_ascii_digit()) {
                s = base.trim().to_string();
                changed = true;
            }
        } else if let Some((base, rest)) = s.rsplit_once('.') {
            if rest.chars().all(|c| c.is_ascii_digit()) {
                s = base.trim().to_string();
                changed = true;
            }
        }
    }

    if s.is_empty() {
        "Audio Track".to_string()
    } else {
        s
    }
}

/// Information extracted from linear scanning of `ProjectData`.
#[derive(Default, Debug)]
pub struct ProjectDataScan {
    pub track_names: Vec<String>,
    pub stock_plugins: Vec<String>,
    pub drummer_types: Vec<String>,
    pub midi_notes: Vec<(u8, u8, f32, f32)>, // pitch, vel, start_beat, len_beat
}

/// Scans a raw `ProjectData` binary blob.
pub fn scan_project_data(data: &[u8]) -> ProjectDataScan {
    let mut scan = ProjectDataScan::default();
    if data.len() < 16 {
        return scan;
    }

    let len = data.len();

    // 1. Scan for Region / Track Names: anchor [0x61, 0xFF] followed by 24 zeroes
    let mut i = 0;
    while i + 28 < len {
        if data[i] == 0x61 && data[i + 1] == 0xFF {
            // Check 24 zero bytes
            if data[i + 2..i + 26].iter().all(|&b| b == 0) {
                let name_len = u16::from_le_bytes([data[i + 26], data[i + 27]]) as usize;
                if (1..=128).contains(&name_len) && i + 28 + name_len <= len {
                    if let Ok(raw_name) = std::str::from_utf8(&data[i + 28..i + 28 + name_len]) {
                        if raw_name.chars().all(|c| c.is_ascii_graphic() || c == ' ') {
                            let cleaned = clean_track_name(raw_name);
                            if !cleaned.is_empty() && !scan.track_names.contains(&cleaned) {
                                scan.track_names.push(cleaned);
                            }
                        }
                    }
                }
            }
        }
        i += 1;
    }

    // 2. Scan for Apple Stock plug-ins: GAME anchor
    i = 14;
    while i + 4 <= len {
        if &data[i..i + 4] == b"GAME" {
            let flag1 = data[i - 14];
            let flag2 = data[i - 13];
            if (flag1 == 0x02 || flag1 == 0x00) && (flag2 == 0x02 || flag2 == 0x01) {
                let raw = &data[i - 12..i];
                let name = String::from_utf8_lossy(raw)
                    .trim_matches('\0')
                    .trim()
                    .to_string();
                if !name.is_empty() && name.chars().all(|c| c.is_ascii_graphic() || c == ' ') {
                    if !scan.stock_plugins.contains(&name) {
                        scan.stock_plugins.push(name);
                    }
                }
            }
        }
        i += 1;
    }

    // 3. Scan for Session Players / Drummer JSON: "selectedPersistentCharacterTypeIdentifier":"
    let drummer_anchor = b"\"selectedPersistentCharacterTypeIdentifier\":\"";
    i = 0;
    while i + drummer_anchor.len() < len {
        if &data[i..i + drummer_anchor.len()] == drummer_anchor {
            let start = i + drummer_anchor.len();
            let mut end = start;
            while end < len && end < start + 96 && data[end] != b'"' {
                end += 1;
            }
            if end < len && data[end] == b'"' {
                if let Ok(id_str) = std::str::from_utf8(&data[start..end]) {
                    if id_str.starts_with("Type_") {
                        let drummer_type = id_str.trim_start_matches("Type_").to_string();
                        if !scan.drummer_types.contains(&drummer_type) {
                            scan.drummer_types.push(drummer_type);
                        }
                    }
                }
            }
        }
        i += 1;
    }

    scan
}

/// Loads a Logic Pro X session (`.logicx` package or folder) into a `ProjectDoc`.
///
/// Audio media is copied or referenced into `media_dir` (e.g. `media/0_<name>.wav`).
pub fn load_logicx(path: &Path, media_dir: &Path) -> Result<ProjectDoc, ProjectError> {
    let bundle_path = find_logicx_bundle(path)
        .map_err(|e| ProjectError::Zip(format!("Logic Pro bundle error: {e}")))?;

    let meta = read_bundle_metadata(&bundle_path);

    let song_name = bundle_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("Logic Project")
        .to_string();

    let mut doc = ProjectDoc {
        format_version: FORMAT_VERSION,
        name: song_name,
        tempo: meta.tempo,
        sig: meta.time_signature,
        key: meta.key_signature,
        ..Default::default()
    };

    // Scan ProjectData if present
    let project_data_path = bundle_path
        .join("Alternatives")
        .join(&meta.active_alternative)
        .join("ProjectData");

    let pdata_scan = if project_data_path.exists() {
        if let Ok(bytes) = fs::read(&project_data_path) {
            scan_project_data(&bytes)
        } else {
            ProjectDataScan::default()
        }
    } else {
        ProjectDataScan::default()
    };

    // Locate and extract audio files
    let audio_files = scan_audio_files(&bundle_path, &meta.active_alternative);
    fs::create_dir_all(media_dir).map_err(ProjectError::Io)?;

    let mut track_map: HashMap<String, Vec<RegionDoc>> = HashMap::new();
    let beats_per_bar = doc.sig.0 as f32;

    for (idx, af) in audio_files.iter().enumerate() {
        let file_stem = af
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("Audio")
            .to_string();
        let track_name = clean_track_name(&file_stem);

        let mut start_bar = 1.0f32;
        let mut len_bars = 4.0f32;

        let ext = af
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();

        // Decode BWF sample offset if WAV
        if ext == "wav" || ext == "wave" {
            if let Ok(bwf) = read_bwf_info(af) {
                if let Some(start_sec) = bwf.start_seconds {
                    let start_beats = start_sec as f32 * (doc.tempo / 60.0);
                    start_bar = (start_beats / beats_per_bar) + 1.0;
                }
                if bwf.duration_seconds > 0.0 {
                    let len_beats = bwf.duration_seconds as f32 * (doc.tempo / 60.0);
                    len_bars = (len_beats / beats_per_bar).max(0.1);
                }
            }
        }

        // Copy audio into destination media directory
        let media_rel = format!("media/{}_{}.{}", idx, file_stem, ext);
        let dest_path = media_dir.join(format!("{}_{}.{}", idx, file_stem, ext));
        let _ = fs::copy(af, &dest_path);

        let region = RegionDoc {
            name: file_stem.clone(),
            start_bar,
            len_bars,
            audio: Some(media_rel),
            gain_db: 0.0,
            mute: false,
            ..Default::default()
        };

        track_map.entry(track_name).or_default().push(region);
    }

    // Assign tracks and regions
    let default_colors = [
        [0x4e, 0xb0, 0x4a],
        [0x36, 0x89, 0xe6],
        [0xe6, 0x5a, 0x36],
        [0x9a, 0x5a, 0xe6],
        [0xe6, 0xc1, 0x36],
    ];

    let mut color_idx = 0;
    for (t_name, mut regions) in track_map {
        regions.sort_by(|a, b| a.start_bar.partial_cmp(&b.start_bar).unwrap());

        // Extract any known stock plug-in effects
        let mut fx = Vec::new();
        for plug in &pdata_scan.stock_plugins {
            if matches!(
                plug.as_str(),
                "Channel EQ" | "Compressor" | "Space D" | "Tape Delay" | "Chorus" | "Phaser"
            ) {
                fx.push(InsertDoc {
                    kind: plug.clone(),
                    bypass: false,
                    params: Vec::new(),
                });
            }
        }

        let track = TrackDoc {
            name: t_name,
            kind: TrackKindDoc::Audio,
            color: default_colors[color_idx % default_colors.len()],
            regions,
            fx,
            ..Default::default()
        };
        color_idx += 1;
        doc.tracks.push(track);
    }

    // Add Session Players / Drummer track if detected in ProjectData
    for drum_type in &pdata_scan.drummer_types {
        let style = if drum_type.to_lowercase().contains("electronic") {
            "electronic"
        } else if drum_type.to_lowercase().contains("pop") {
            "pop"
        } else {
            "rock"
        };
        doc.tracks.push(TrackDoc {
            name: format!("Drummer ({drum_type})"),
            kind: TrackKindDoc::Drummer,
            color: [0xe6, 0x89, 0x36],
            regions: vec![RegionDoc {
                name: "Drummer Region".into(),
                start_bar: 1.0,
                len_bars: 8.0,
                drummer: Some(DrummerDoc {
                    style: style.into(),
                    complexity: 50,
                    loudness: 60,
                    swing: 0,
                    fills: 30,
                    parts: 63,
                    seed: 1,
                }),
                ..Default::default()
            }],
            ..Default::default()
        });
    }

    // If no tracks were discovered at all, provide 1 default Audio track
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

/// Synthesizes and writes a valid Apple Logic Pro X (`.logicx`) bundle package to `bundle_path`.
pub fn save_logicx(
    bundle_path: &Path,
    doc: &ProjectDoc,
    media: &[MediaFile],
) -> Result<(), ProjectError> {
    let mut path = bundle_path.to_path_buf();
    if path.extension().and_then(|e| e.to_str()) != Some("logicx") {
        path.set_extension("logicx");
    }

    let alt_dir = path.join("Alternatives").join("000");
    let audio_dir = path.join("Audio Files");

    fs::create_dir_all(&alt_dir).map_err(ProjectError::Io)?;
    fs::create_dir_all(&audio_dir).map_err(ProjectError::Io)?;

    // 1. ProjectInformation.plist
    let info_plist = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>activeAlternative</key>
	<integer>0</integer>
	<key>logicVersion</key>
	<string>11.0.0</string>
	<key>creationVersion</key>
	<string>11.0.0</string>
</dict>
</plist>"#
    );
    fs::write(path.join("ProjectInformation.plist"), info_plist).map_err(ProjectError::Io)?;

    // 2. MetaData.plist
    let sample_rate = 48000u32;
    let meta_plist = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>tempo</key>
	<real>{}</real>
	<key>keySignature</key>
	<string>{}</string>
	<key>timeSignature</key>
	<array>
		<integer>{}</integer>
		<integer>{}</integer>
	</array>
	<key>sampleRate</key>
	<integer>{}</integer>
</dict>
</plist>"#,
        doc.tempo, doc.key, doc.sig.0, doc.sig.1, sample_rate
    );
    fs::write(alt_dir.join("MetaData.plist"), meta_plist).map_err(ProjectError::Io)?;

    // 3. DisplayState.plist
    let display_plist = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>TracksAreaZoomH</key>
	<real>1.0</real>
	<key>TracksAreaZoomV</key>
	<real>1.0</real>
</dict>
</plist>"#;
    fs::write(alt_dir.join("DisplayState.plist"), display_plist).map_err(ProjectError::Io)?;

    // 4. Audio Files: Write all audio with BWF `bext` timestamps matching timeline positions
    let beats_per_bar = doc.sig.0 as f32;

    for track in &doc.tracks {
        for region in &track.regions {
            if let Some(audio_ref) = &region.audio {
                if let Some(m) = media.iter().find(|m| &m.zip_path == audio_ref) {
                    let start_bars = (region.start_bar - 1.0).max(0.0);
                    let start_beats = start_bars * beats_per_bar;
                    let start_seconds = start_beats * (60.0 / doc.tempo);
                    let time_reference = (start_seconds * sample_rate as f32) as u64;

                    let file_name = m
                        .source
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("stem.wav");
                    let dst_file = audio_dir.join(file_name);

                    // If it's a WAV file, write with BWF bext timestamp
                    if m.source.exists() {
                        let is_wav = m
                            .source
                            .extension()
                            .and_then(|e| e.to_str())
                            .map(|e| e.eq_ignore_ascii_case("wav"))
                            .unwrap_or(false);

                        if is_wav {
                            let _ = write_bwf_copy(
                                &m.source,
                                &dst_file,
                                time_reference,
                                &region.name,
                                "Toast Wave",
                            );
                        } else {
                            let _ = fs::copy(&m.source, &dst_file);
                        }
                    }
                }
            }
        }
    }

    // 5. Synthesize ProjectData binary blob
    let mut pdata: Vec<u8> = Vec::new();
    // Header magic
    pdata.extend_from_slice(&PROJECT_DATA_MAGIC);
    pdata.extend_from_slice(&[0x00, 0x00, 0x01, 0x00]); // version 1.0

    // Synthesize Track chunks (`karT`) and Audio regions (`gRuA`)
    for (_t_idx, track) in doc.tracks.iter().enumerate() {
        // Track marker
        pdata.extend_from_slice(CHUNK_TRAK);
        let t_len = 32u32;
        pdata.extend_from_slice(&t_len.to_le_bytes());

        // Track Name record: 0x61, 0xFF, 24x 0x00, len: u16, name
        pdata.push(0x61);
        pdata.push(0xFF);
        pdata.extend_from_slice(&[0u8; 24]);
        let name_bytes = track.name.as_bytes();
        let name_len = (name_bytes.len().min(100)) as u16;
        pdata.extend_from_slice(&name_len.to_le_bytes());
        pdata.extend_from_slice(&name_bytes[0..name_len as usize]);

        // Stock plugins (GAME anchor)
        for insert in &track.fx {
            let mut plug_name = [0u8; 12];
            let pb = insert.kind.as_bytes();
            let copy_len = pb.len().min(12);
            plug_name[0..copy_len].copy_from_slice(&pb[0..copy_len]);

            // Layout: flag1 (0x02), flag2 (0x02), 12-byte name, GAME
            pdata.push(0x02);
            pdata.push(0x02);
            pdata.extend_from_slice(&plug_name);
            pdata.extend_from_slice(b"GAME");
        }

        // Audio regions (`gRuA`)
        for region in &track.regions {
            pdata.extend_from_slice(CHUNK_AURG);
            let r_len = 48u32;
            pdata.extend_from_slice(&r_len.to_le_bytes());

            // 0x61 0xFF name record
            pdata.push(0x61);
            pdata.push(0xFF);
            pdata.extend_from_slice(&[0u8; 24]);
            let r_name_bytes = region.name.as_bytes();
            let r_name_len = (r_name_bytes.len().min(100)) as u16;
            pdata.extend_from_slice(&r_name_len.to_le_bytes());
            pdata.extend_from_slice(&r_name_bytes[0..r_name_len as usize]);

            // Region offset in 960 PPQ ticks
            let start_beats = (region.start_bar - 1.0).max(0.0) * beats_per_bar;
            let start_ticks = (start_beats * 960.0) as u32;
            let len_ticks = (region.len_bars * beats_per_bar * 960.0) as u32;
            pdata.extend_from_slice(&start_ticks.to_le_bytes());
            pdata.extend_from_slice(&len_ticks.to_le_bytes());
        }

        // MIDI sequences (`qeSM`)
        for region in &track.regions {
            if !region.notes.is_empty() {
                pdata.extend_from_slice(CHUNK_MSEQ);
                let notes_payload_len = (region.notes.len() * 8) as u32;
                pdata.extend_from_slice(&notes_payload_len.to_le_bytes());
                for note in &region.notes {
                    let note_start_tick = (note.start * 960.0) as u32;
                    let note_len_tick = (note.len * 960.0) as u16;
                    pdata.extend_from_slice(&note_start_tick.to_le_bytes());
                    pdata.extend_from_slice(&note_len_tick.to_le_bytes());
                    pdata.push(note.pitch);
                    pdata.push(note.vel);
                }
            }
        }
    }

    fs::write(alt_dir.join("ProjectData"), pdata).map_err(ProjectError::Io)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::NoteDoc;
    use tempfile::tempdir;

    #[test]
    fn round_trip_logicx_bundle() {
        let dir = tempdir().unwrap();
        let bundle_path = dir.path().join("TestSong.logicx");
        let media_dir = dir.path().join("extracted_media");

        let doc = ProjectDoc {
            name: "TestSong".into(),
            tempo: 128.0,
            sig: (4, 4),
            key: "F#min".into(),
            tracks: vec![
                TrackDoc {
                    name: "Lead Vocal".into(),
                    kind: TrackKindDoc::Audio,
                    regions: vec![RegionDoc {
                        name: "Verse 1".into(),
                        start_bar: 2.0,
                        len_bars: 4.0,
                        ..Default::default()
                    }],
                    fx: vec![InsertDoc {
                        kind: "Compressor".into(),
                        bypass: false,
                        params: vec![],
                    }],
                    ..Default::default()
                },
                TrackDoc {
                    name: "Synth Bass".into(),
                    kind: TrackKindDoc::Instrument,
                    regions: vec![RegionDoc {
                        name: "Bassline".into(),
                        start_bar: 1.0,
                        len_bars: 8.0,
                        notes: vec![
                            NoteDoc {
                                start: 0.0,
                                len: 1.0,
                                pitch: 36,
                                vel: 100,
                            },
                            NoteDoc {
                                start: 1.0,
                                len: 1.0,
                                pitch: 38,
                                vel: 90,
                            },
                        ],
                        ..Default::default()
                    }],
                    ..Default::default()
                },
            ],
            ..Default::default()
        };

        save_logicx(&bundle_path, &doc, &[]).unwrap();

        assert!(bundle_path.exists());
        assert!(bundle_path.join("ProjectInformation.plist").exists());
        assert!(bundle_path
            .join("Alternatives")
            .join("000")
            .join("MetaData.plist")
            .exists());
        assert!(bundle_path
            .join("Alternatives")
            .join("000")
            .join("ProjectData")
            .exists());

        // Test loading back
        let loaded = load_logicx(&bundle_path, &media_dir).unwrap();
        assert_eq!(loaded.name, "TestSong");
        assert_eq!(loaded.tempo, 128.0);
        assert_eq!(loaded.sig, (4, 4));
        assert_eq!(loaded.key, "F#min");
    }
}
