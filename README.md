# MS Toolkit

MS Toolkit is a Rust desktop application for loading, reviewing, and exporting GPS movement data from field logs and SYLK spreadsheets. It uses `egui` for the UI and OpenStreetMap tiles for map views.

## What It Does

- Loads GPS fixes from local files or from a remote server over SFTP
- Parses MultiSpeak-style `rxLocationReport` log lines into per-source breadcrumb trails
- Parses `.slk` spreadsheets grouped by vehicle name
- Removes consecutive duplicate positions within a small coordinate tolerance
- Highlights large jumps per source ID using a green / yellow / red severity scale
- Opens an interactive breadcrumb map for any loaded ID
- Exports loaded data to HTML, PDF, or KMZ for sharing outside the app

## Supported Inputs

Local file picker:

- `.log`
- `.txt`
- `.slk`
- `.tar`
- `.tbz2`

SFTP browser:

- `.log`
- `.log.<date>` dated log variants such as `system.log.20260410`
- `.txt`
- `.slk`
- `.tar`
- `.tbz2`

Archive behavior:

- `.tar` and `.tbz2` inputs are opened and the first log-like file inside the archive is parsed

## Expected Data Formats

### MultiSpeak-style log lines

The log parser looks for lines matching this pattern:

```text
MM/DD/YY HH:MM:SS.mmm [...] rxLocationReport targetId: () sourceId: (<id>) latitude: (<lat>) longitude: (<lon>)
```

### SYLK spreadsheets

For `.slk` files, the parser expects a header row with these columns:

- `VehicleName`
- `EventTime`
- `Lat`
- `Lon`

If those headers are missing, the app falls back to the default column positions currently used by the parser.

## Main Workflow

1. Build and launch `ms_toolkit.exe`.
2. Load data with **Open** or use **SFTP** to browse and download a supported remote file.
3. Review source IDs in the left sidebar.
4. Use search to filter IDs and `Ctrl+Click` to create a multi-selection.
5. Open a live map for a specific ID, or export all IDs or only the current selection.

## Exports

- `HTML`: self-contained report with summary tables and Leaflet-based maps
- `PDF`: printable report with summary pages and generated map views
- `KMZ`: Google Earth compatible archive containing KML tracks and waypoints

## Jump Detection And Deduplication

- Consecutive fixes with effectively identical coordinates are removed during parsing
- Large jumps are counted per source ID
- The current threshold is `0.05` degrees, roughly `5.5 km` at mid-latitudes
- Sidebar colors indicate jump severity:
	- Green: `0`
	- Yellow: `1-15`
	- Red: `>15`

## Building

Requirements:

- Rust stable toolchain
- A working native C/C++ build toolchain for crate dependencies

Build the release binary with:

```bash
cargo build --release
```

Output:

- `target/release/ms_toolkit.exe`

## SFTP Notes

- The SFTP browser supports directory navigation and direct download into the app
- Downloaded `.tar` and `.tbz2` files are extracted in memory before parsing
- The current SSH client accepts all host keys without verification

The last point is convenient for internal use, but it is not appropriate for hostile or untrusted networks.

## Crates Used

| Crate | Purpose |
|---|---|
| `eframe` / `egui` | Desktop GUI |
| `egui_extras` | Tables and extra UI widgets |
| `walkers` | Embedded OpenStreetMap tiles |
| `rayon` | Parallel parsing and dedup work |
| `regex` | Log line extraction |
| `rfd` | Native file dialogs |
| `printpdf` | PDF generation |
| `zip` | KMZ packaging |
| `bzip2` / `tar` | `.tbz2` and `.tar` extraction |
| `russh` / `russh-sftp` | SFTP transport |
| `tokio` | Async runtime |

## Status Bar Summary

After a file is loaded, the app reports:

- Number of source IDs found
- Number of fixes kept after deduplication
- Number of duplicate positions removed
- Source file path or downloaded filename
