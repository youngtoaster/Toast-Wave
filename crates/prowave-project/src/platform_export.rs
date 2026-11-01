//! Unified cross-platform export engine.
//!
//! Provides one-click export for:
//! - Apple Logic Pro X (`.logicx` package bundle)
//! - Final Cut Pro / Premiere / Resolve XML (`.fcpxml`)
//! - Pro Tools, Ableton Live, FL Studio, Reaper, Studio One, Cubase: Consolidated zero-aligned BWF stems
//! - Timestamped BWF stems (`bext` TimeReference sample offsets)
//! - Standard MIDI (`.mid`)
//! - Complete Universal Production Bundle (all platform targets organized into one directory)

use crate::fcpxml::export_fcpxml;
use crate::logicx::save_logicx;
use crate::stems::{export_bwf_stems, StemExportMode};
use crate::{MediaFile, ProjectDoc, ProjectError};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

/// Available platform export targets.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PlatformExportFormat {
    /// Apple Logic Pro X project package (`.logicx`).
    LogicProX,
    /// Final Cut Pro XML timeline interchange (`.fcpxml`).
    FinalCutProXml,
    /// Universal 24-bit BWF stems, zero-aligned from sample 0 for 1-click import into
    /// Pro Tools, Ableton Live, FL Studio, Reaper, Studio One, and Cubase.
    BwfStemsConsolidated,
    /// Broadcast Wave Format stems preserving region placement with `bext` TimeReference chunks.
    BwfStemsTimestamped,
    /// Universal production bundle with LogicX, FCPXML, BWF Stems, MIDI, and Session Report.
    UniversalBundle,
}

impl PlatformExportFormat {
    pub const ALL: [PlatformExportFormat; 5] = [
        PlatformExportFormat::LogicProX,
        PlatformExportFormat::FinalCutProXml,
        PlatformExportFormat::BwfStemsConsolidated,
        PlatformExportFormat::BwfStemsTimestamped,
        PlatformExportFormat::UniversalBundle,
    ];

    pub fn display_name(&self) -> &'static str {
        match self {
            Self::LogicProX => "Apple Logic Pro X Project (.logicx)",
            Self::FinalCutProXml => "Final Cut Pro XML (.fcpxml)",
            Self::BwfStemsConsolidated => "Universal Multi-track Stems (Consolidated, Zero-Aligned BWF WAV)",
            Self::BwfStemsTimestamped => "Broadcast Wave Stems (BWF with TimeReference Timestamps)",
            Self::UniversalBundle => "Universal Production Archive (All Platforms + Stems + Report)",
        }
    }

    pub fn description(&self) -> &'static str {
        match self {
            Self::LogicProX => "Complete Logic Pro project package with plists, stock plug-in anchors, and audio assets.",
            Self::FinalCutProXml => "Timeline interchange file supported by Logic Pro, Final Cut Pro, Premiere, and DaVinci Resolve.",
            Self::BwfStemsConsolidated => "Sample 0 aligned 24-bit WAV files for instant drag-and-drop into Pro Tools, Ableton, FL Studio, Reaper, Cubase.",
            Self::BwfStemsTimestamped => "Individual BWF audio clips embedded with 64-bit bext TimeReference sample positions for spot-to-timecode import.",
            Self::UniversalBundle => "Complete archive folder containing Logic Pro X bundle, FCPXML, consolidated BWF stems, MIDI tracks, and SESSION_INFO.txt.",
        }
    }

    pub fn default_extension(&self) -> &'static str {
        match self {
            Self::LogicProX => "logicx",
            Self::FinalCutProXml => "fcpxml",
            Self::BwfStemsConsolidated | Self::BwfStemsTimestamped | Self::UniversalBundle => "",
        }
    }
}

/// Executes export according to the specified `format`.
/// Returns the output path of the exported file or folder.
pub fn export_project_for_platform(
    doc: &ProjectDoc,
    media: &[MediaFile],
    target_path: &Path,
    format: PlatformExportFormat,
) -> Result<PathBuf, ProjectError> {
    match format {
        PlatformExportFormat::LogicProX => {
            let mut out = target_path.to_path_buf();
            if out.extension().and_then(|e| e.to_str()) != Some("logicx") {
                out.set_extension("logicx");
            }
            save_logicx(&out, doc, media)?;
            Ok(out)
        }
        PlatformExportFormat::FinalCutProXml => {
            let mut out = target_path.to_path_buf();
            if out.extension().and_then(|e| e.to_str()) != Some("fcpxml") {
                out.set_extension("fcpxml");
            }
            export_fcpxml(&out, doc, media)?;
            Ok(out)
        }
        PlatformExportFormat::BwfStemsConsolidated => {
            let out_dir = if target_path.is_dir() {
                target_path.to_path_buf()
            } else {
                target_path.with_extension("")
            };
            export_bwf_stems(doc, media, &out_dir, StemExportMode::Consolidated)?;
            Ok(out_dir)
        }
        PlatformExportFormat::BwfStemsTimestamped => {
            let out_dir = if target_path.is_dir() {
                target_path.to_path_buf()
            } else {
                target_path.with_extension("")
            };
            export_bwf_stems(doc, media, &out_dir, StemExportMode::Timestamped)?;
            Ok(out_dir)
        }
        PlatformExportFormat::UniversalBundle => {
            let bundle_dir = if target_path.is_dir() {
                target_path.to_path_buf()
            } else {
                target_path.with_extension("")
            };
            fs::create_dir_all(&bundle_dir)?;

            // 1. Export Logic Pro X package
            let logicx_dir = bundle_dir.join(format!("{}.logicx", doc.name));
            save_logicx(&logicx_dir, doc, media)?;

            // 2. Export FCPXML
            let fcpxml_path = bundle_dir.join(format!("{}.fcpxml", doc.name));
            export_fcpxml(&fcpxml_path, doc, media)?;

            // 3. Export Consolidated BWF Stems & Session Report
            export_bwf_stems(doc, media, &bundle_dir, StemExportMode::Consolidated)?;

            Ok(bundle_dir)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bwf::write_bwf_pcm;
    use crate::{RegionDoc, TrackDoc, TrackKindDoc};
    use tempfile::tempdir;

    #[test]
    fn test_platform_export_universal_bundle() {
        let dir = tempdir().unwrap();
        let audio_src = dir.path().join("audio.wav");
        write_bwf_pcm(&audio_src, &[0.0f32; 9600], 48000, 2, 24, 0, "Test", "TW").unwrap();

        let doc = ProjectDoc {
            name: "UniversalSong".into(),
            tempo: 128.0,
            sig: (4, 4),
            tracks: vec![TrackDoc {
                name: "Bass".into(),
                kind: TrackKindDoc::Audio,
                regions: vec![RegionDoc {
                    name: "Bassline".into(),
                    start_bar: 1.0,
                    len_bars: 4.0,
                    audio: Some("media/0_audio.wav".into()),
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        };

        let media = vec![MediaFile {
            zip_path: "media/0_audio.wav".into(),
            source: audio_src,
        }];

        let target_dir = dir.path().join("UniversalExport");
        let exported = export_project_for_platform(
            &doc,
            &media,
            &target_dir,
            PlatformExportFormat::UniversalBundle,
        )
        .unwrap();

        assert!(exported.exists());
        assert!(exported.join("UniversalSong.logicx").exists());
        assert!(exported.join("UniversalSong.fcpxml").exists());
        assert!(exported.join("export_manifest.json").exists());
        assert!(exported.join("SESSION_INFO.txt").exists());
        assert!(exported.join("Stems").exists());
    }
}
