## Toast Wave v0.3.1 - Enhanced Logic Pro X Ingestion & AppleDouble Resolution

### What's Changed
- **AppleDouble (`._*`) Sidecar Handling**: Automatically strips macOS `._` prefixes (e.g. `._DEJA VU-UNAPOLOGETIC.logicx`) and seamlessly resolves to the actual sibling project bundle directory.
- **Extended Logic Pro Bundle Detection**:
  - Supports `Resources/ProjectInformation.plist` in addition to root `ProjectInformation.plist`.
  - Full Folder Mode support (`.musicapps-project-folder` detection and scanning parent/sibling directories for bundles).
- **AIFF (`.aif`/`.aiff`) Engine & Metadata Parser**:
  - Implemented IEEE 754 80-bit extended precision float sample rate parser (`COMM` chunk).
  - Apple `MARK` and `ARK\x00` timestamp decoder: accurately converts Logic Pro SMPTE sample offsets into timeline bar positions and durations.
  - Active audio files prioritization using `MetaData.plist` (`AudioFiles` and `UnusedAudioFiles`).
- **Clean Track Grouping**: Intelligently groups multi-take recordings (e.g. `DEJA VU-UNAPOLOGETIC_5#16_1.aif`) onto consolidated audio tracks (e.g. `Audio 5`) positioned along the timeline.
- **File Dialog Enhancements**: Added `All Files (*.*)` filter and drag-and-drop bundle opening.

### Assets
- `prowave-windows-x86_64.zip`: Standalone Windows 64-bit release build (`prowave.exe`).
