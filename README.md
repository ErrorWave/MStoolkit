# MS Toolkit

A desktop GUI application for analyzing GPS location data from MultiSpeak log files. Built with Rust + egui.

## Features

- **Log parsing** — Opens `.log` files and extracts all `rxLocationReport` GPS fixes using parallel regex matching (rayon)
- **Deduplication** — Consecutive fixes for the same source ID with identical coordinates (within ~1 m) are automatically removed
- **Jump detection** — Flags large position jumps (> 0.05° ≈ 5.5 km) per source ID, color-coded green / yellow / red
- **Live map** — Click any source ID to open a breadcrumb trail on an interactive OpenStreetMap map
- **Multi-select** — Ctrl+Click to select multiple source IDs in the sidebar
- **HTML report** — Export selected IDs to a self-contained HTML report with Leaflet maps, a summary table, and per-ID stats
- **Search** — Filter the source ID list by ID number

## Log format

The parser expects lines in the following form:

```
MM/DD/YY HH:MM:SS.mmm [...] rxLocationReport targetId: () sourceId: (<id>) latitude: (<lat>) longitude: (<lon>)
```

See [example.txt](example.txt) for a representative sample.

## Building

Requires Rust (stable) and a working C toolchain.

```bash
cargo build --release
```

The release binary is written to `target/release/ms_toolkit.exe`.

## Usage

1. Launch `ms_toolkit.exe`
2. Click **Open** and select a `.log` file
3. Source IDs appear in the left sidebar — click to focus, Ctrl+Click to multi-select
4. Click **Map** next to any ID to view its breadcrumb trail
5. With one or more IDs selected, click **Export HTML** to generate a report

## Dependencies

| Crate | Purpose |
|---|---|
| `eframe` / `egui` | Immediate-mode GUI |
| `egui_extras` | Table widget |
| `walkers` | Embedded OpenStreetMap tiles |
| `rayon` | Parallel log parsing |
| `regex` | GPS line extraction |
| `rfd` | Native file-open dialog |
| `printpdf` | PDF export |
| `tokio` | Async tile fetching |
