// Created by Owen Hammond & Baker's Communications LLC
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod parser;

use eframe::{egui, App, CreationContext, Frame};
use egui::{Color32, ScrollArea, Stroke};
use egui_extras::{Column, TableBuilder};
use parser::{parse_log, parse_slk, GpsRecord};
use std::collections::{HashMap, HashSet};
use std::sync::mpsc::{self, Receiver};
use walkers::{HttpTiles, Map, MapMemory, Plugin, Position, Projector, Tiles, TileId};
use walkers::sources::{Attribution, TileSource};

// CartoDB Voyager tiles — no API key, permissive usage policy
struct CartoVoyager;
impl TileSource for CartoVoyager {
    fn tile_url(&self, tile: TileId) -> String {
        format!(
            "https://a.basemaps.cartocdn.com/rastertiles/voyager/{}/{}/{}.png",
            tile.zoom, tile.x, tile.y
        )
    }
    fn attribution(&self) -> Attribution {
        Attribution {
            text: "© OpenStreetMap contributors © CARTO",
            url: "https://carto.com/attributions",
            logo_light: None,
            logo_dark: None,
        }
    }
}

// ── Jump detection ────────────────────────────────────────────────────────────

/// 0.05° ≈ 5.5 km at mid-latitudes.
const JUMP_THRESHOLD_DEG: f64 = 0.05;

fn count_large_jumps(recs: &[GpsRecord]) -> usize {
    recs.windows(2)
        .filter(|w| {
            let dlat = w[1].lat - w[0].lat;
            let dlon = w[1].lon - w[0].lon;
            (dlat * dlat + dlon * dlon).sqrt() > JUMP_THRESHOLD_DEG
        })
        .count()
}

fn jump_color(jumps: usize) -> Color32 {
    match jumps {
        0 => Color32::from_rgb(80, 200, 80),
        1..=15 => Color32::from_rgb(220, 195, 40),
        _ => Color32::from_rgb(210, 55, 55),
    }
}

// ── Breadcrumb map plugin (egui live map) ─────────────────────────────────────

struct Breadcrumbs {
    points: Vec<(f64, f64)>,
}

impl Plugin for Breadcrumbs {
    fn run(
        self: Box<Self>,
        ui: &mut egui::Ui,
        response: &egui::Response,
        projector: &Projector,
    ) {
        if self.points.is_empty() {
            return;
        }
        let painter = ui.painter().with_clip_rect(response.rect);
        let pts: Vec<egui::Pos2> = self
            .points
            .iter()
            .map(|&(lat, lon)| {
                let v = projector.project(Position::from_lat_lon(lat, lon));
                egui::pos2(v.x, v.y)
            })
            .collect();

        for w in pts.windows(2) {
            painter.line_segment([w[0], w[1]], Stroke::new(2.5, Color32::from_rgb(30, 110, 230)));
        }
        let n = pts.len();
        for (i, &pt) in pts.iter().enumerate() {
            let (fill, radius) = match i {
                0 => (Color32::GREEN, 7.0_f32),
                _ if i == n - 1 => (Color32::RED, 7.0_f32),
                _ => (Color32::from_rgb(255, 140, 0), 5.0_f32),
            };
            painter.circle_filled(pt, radius, fill);
            painter.circle_stroke(pt, radius, Stroke::new(1.5, Color32::WHITE));
        }
    }
}

// ── Per-ID map window state ───────────────────────────────────────────────────

struct MapWin {
    memory: MapMemory,
    center: Position,
    open: bool,
}

// ── Background load result ────────────────────────────────────────────────────

struct LoadResult {
    path: String,
    records: HashMap<String, Vec<GpsRecord>>,
    raw_fixes: usize,
    after_dedup: usize,
}

// ── Application state ─────────────────────────────────────────────────────────

struct GpsApp {
    records: HashMap<String, Vec<GpsRecord>>,
    sorted_ids: Vec<String>,
    jump_counts: HashMap<String, usize>,
    search: String,
    /// All IDs highlighted in the sidebar (Ctrl+Click for multi-select).
    selection: HashSet<String>,
    /// The ID whose detail panel is shown in the central area.
    focused: Option<String>,
    status: String,
    is_loading: bool,
    load_rx: Option<Receiver<Result<LoadResult, String>>>,
    tiles: Option<HttpTiles>,
    maps: HashMap<String, MapWin>,
}

impl GpsApp {
    fn new(cc: &CreationContext<'_>) -> Self {
        let tiles = HttpTiles::new(CartoVoyager, cc.egui_ctx.clone());
        Self {
            records: HashMap::new(),
            sorted_ids: Vec::new(),
            jump_counts: HashMap::new(),
            search: String::new(),
            selection: HashSet::new(),
            focused: None,
            status: "Open a .log or .slk file to begin.".into(),
            is_loading: false,
            load_rx: None,
            tiles: Some(tiles),
            maps: HashMap::new(),
        }
    }

    fn start_load(&mut self, path: String, ctx: egui::Context) {
        let (tx, rx) = mpsc::channel();
        self.load_rx = Some(rx);
        self.is_loading = true;
        self.status = format!("Loading {}…", path);

        let is_slk = path.to_lowercase().ends_with(".slk");
        std::thread::spawn(move || {
            let result = match std::fs::read_to_string(&path) {
                Ok(text) => {
                    let (records, stats) = if is_slk {
                        parse_slk(&text)
                    } else {
                        parse_log(&text)
                    };
                    Ok(LoadResult {
                        path,
                        records,
                        raw_fixes: stats.raw_fixes,
                        after_dedup: stats.after_dedup,
                    })
                }
                Err(e) => Err(format!("Error reading file: {e}")),
            };
            let _ = tx.send(result);
            ctx.request_repaint();
        });
    }

    fn apply_load_result(&mut self, result: LoadResult) {
        let mut ids: Vec<String> = result.records.keys().cloned().collect();
        ids.sort_by_key(|s| s.parse::<u64>().unwrap_or(u64::MAX));
        let removed = result.raw_fixes.saturating_sub(result.after_dedup);
        self.status = format!(
            "{} source IDs  |  {} fixes  |  {} duplicate positions removed  |  {}",
            ids.len(),
            result.after_dedup,
            removed,
            result.path,
        );
        self.jump_counts = result
            .records
            .iter()
            .map(|(id, recs)| (id.clone(), count_large_jumps(recs)))
            .collect();
        self.sorted_ids = ids;
        self.records = result.records;
        self.maps.clear();
        self.selection.clear();
        self.focused = None;
    }

    // ── Selection helpers ─────────────────────────────────────────────────────

    /// IDs from `sorted_ids` that are in the current selection (preserves sort order).
    fn selected_ids_sorted(&self) -> Vec<String> {
        self.sorted_ids
            .iter()
            .filter(|id| self.selection.contains(*id))
            .cloned()
            .collect()
    }

    // ── HTML report ───────────────────────────────────────────────────────────

    fn build_html_report(&mut self, ids: Vec<String>) {
        if ids.is_empty() {
            return;
        }

        let total_fixes: usize = ids
            .iter()
            .filter_map(|id| self.records.get(id))
            .map(|v| v.len())
            .sum();

        let style = r#"
            body{font-family:Arial,Helvetica,sans-serif;margin:0;background:#f5f7fa;color:#222}
            header{background:#1a2a4a;color:#fff;padding:18px 32px}
            header h1{margin:0 0 4px;font-size:1.4em}
            header p{margin:0;opacity:.75;font-size:.88em}
            .wrap{max-width:1100px;margin:0 auto;padding:24px 32px}
            .summary{background:#fff;border:1px solid #d8dde4;border-radius:6px;padding:18px 22px;margin-bottom:28px}
            .summary h2{margin:0 0 12px;font-size:1em;color:#1a2a4a;text-transform:uppercase;letter-spacing:.05em}
            table{border-collapse:collapse;width:100%}
            th{text-align:left;padding:7px 10px;background:#edf1f7;font-size:.82em;color:#555;border-bottom:2px solid #c8d4e0}
            td{padding:6px 10px;border-bottom:1px solid #eef;font-size:.86em}
            tr:last-child td{border-bottom:none}
            a{color:#1a5cbf;text-decoration:none}a:hover{text-decoration:underline}
            .card{background:#fff;border:1px solid #d8dde4;border-radius:6px;margin-bottom:28px;overflow:hidden}
            .card-hd{display:flex;align-items:center;gap:14px;padding:11px 22px;background:#edf1f7;border-bottom:1px solid #d8dde4}
            .card-hd h2{margin:0;font-size:1em;color:#1a2a4a}
            .badge{padding:2px 10px;border-radius:12px;font-size:.78em;font-weight:700;color:#fff}
            .card-body{display:flex;flex-wrap:wrap}
            .stats{padding:16px 22px;min-width:260px}
            .stats table{width:auto}
            .stats th{background:none;border:none;color:#888;font-size:.8em;padding:3px 12px 3px 0;font-weight:700}
            .stats td{border:none;padding:3px 0;font-size:.86em}
            .mapbox{padding:16px 22px;flex:1;min-width:380px}
            .leaflet-container{height:360px;border-radius:6px;border:1px solid #c8d4e0}
        "#;

        let mut html = format!(
            r#"<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width,initial-scale=1">
  <title>GPS Log Report</title>
  <link rel="stylesheet" href="https://unpkg.com/leaflet@1.9.4/dist/leaflet.css"/>
  <script src="https://unpkg.com/leaflet@1.9.4/dist/leaflet.js"></script>
  <style>{style}</style>
</head>
<body>
<header>
  <h1>GPS Log Report</h1>
  <p>{id_count} source ID(s)&ensp;&bull;&ensp;{total_fixes} total fixes (after dedup)</p>
</header>
<div class="wrap">
"#,
            id_count = ids.len(),
        );

        // ── Summary table ─────────────────────────────────────────────────────
        html.push_str(
            r#"<div class="summary"><h2>Summary</h2>
<table><thead><tr>
  <th>Source ID</th><th>Fixes</th><th>Large Jumps</th>
  <th>First Fix</th><th>Last Fix</th><th>Last Position</th>
</tr></thead><tbody>
"#,
        );
        for id in &ids {
            if let Some(recs) = self.records.get(id) {
                let jumps = self.jump_counts.get(id).copied().unwrap_or(0);
                let first_ts = recs.first().map(|r| r.timestamp.as_str()).unwrap_or("-");
                let last_ts = recs.last().map(|r| r.timestamp.as_str()).unwrap_or("-");
                let last_lat = recs.last().map(|r| r.lat).unwrap_or(0.0);
                let last_lon = recs.last().map(|r| r.lon).unwrap_or(0.0);
                let jcol = match jumps {
                    0 => "#27ae60",
                    1..=15 => "#c97d10",
                    _ => "#c0392b",
                };
                let anchor = format!("#src-{id}");
                html.push_str(&format!(
                    r#"<tr><td><a href="{anchor}">{id}</a></td><td>{fixes}</td><td style="color:{jcol};font-weight:700">{jumps}</td><td>{first_ts}</td><td>{last_ts}</td><td>{last_lat:.6}, {last_lon:.6}</td></tr>
"#,
                    fixes = recs.len(),
                ));
            }
        }
        html.push_str("</tbody></table></div>\n");

        // ── Per-ID cards + Leaflet JS ─────────────────────────────────────────
        let mut map_scripts = String::new();

        for id in &ids {
            if let Some(recs) = self.records.get(id) {
                let jumps = self.jump_counts.get(id).copied().unwrap_or(0);
                let (badge_col, jump_label) = match jumps {
                    0 => ("#27ae60", "No large jumps".to_string()),
                    1 => ("#c97d10", "1 large jump".to_string()),
                    n => ("#c0392b", format!("{n} large jumps")),
                };
                let first_ts = recs.first().map(|r| r.timestamp.as_str()).unwrap_or("-");
                let last_ts = recs.last().map(|r| r.timestamp.as_str()).unwrap_or("-");
                let last_lat = recs.last().map(|r| r.lat).unwrap_or(0.0);
                let last_lon = recs.last().map(|r| r.lon).unwrap_or(0.0);
                let map_id = format!("map-{id}");

                html.push_str(&format!(
                    r#"<div class="card" id="src-{id}">
  <div class="card-hd">
    <h2>Source ID: {id}</h2>
    <span class="badge" style="background:{badge_col}">{jump_label}</span>
  </div>
  <div class="card-body">
    <div class="stats"><table>
      <tr><th>Total fixes</th><td>{fixes}</td></tr>
      <tr><th>Large jumps</th><td>{jumps}</td></tr>
      <tr><th>First fix</th><td>{first_ts}</td></tr>
      <tr><th>Last fix</th><td>{last_ts}</td></tr>
      <tr><th>Last latitude</th><td>{last_lat:.6}</td></tr>
      <tr><th>Last longitude</th><td>{last_lon:.6}</td></tr>
    </table></div>
    <div class="mapbox"><div id="{map_id}" style="height:360px;border-radius:6px;border:1px solid #c8d4e0"></div></div>
  </div>
</div>
"#,
                    fixes = recs.len(),
                ));

                // Build compact LatLng array: [[lat,lon],...]
                let coords_json: String = {
                    let mut s = String::with_capacity(recs.len() * 24);
                    s.push('[');
                    for (i, r) in recs.iter().enumerate() {
                        if i > 0 {
                            s.push(',');
                        }
                        s.push_str(&format!("[{:.6},{:.6}]", r.lat, r.lon));
                    }
                    s.push(']');
                    s
                };

                let clat = recs.last().map(|r| r.lat).unwrap_or(0.0);
                let clon = recs.last().map(|r| r.lon).unwrap_or(0.0);

                map_scripts.push_str(&format!(
                    r#"(function(){{
  var m=L.map('{map_id}').setView([{clat:.6},{clon:.6}],13);
  L.tileLayer('https://{{s}}.basemaps.cartocdn.com/rastertiles/voyager/{{z}}/{{x}}/{{y}}{{r}}.png',{{
    attribution:'&copy; <a href="https://www.openstreetmap.org/copyright">OpenStreetMap</a> contributors &copy; <a href="https://carto.com/attributions">CARTO</a>',
    subdomains:'abcd',
    maxZoom:20
  }}).addTo(m);
  var c={coords_json};
  if(c.length>0){{
    var pl=L.polyline(c,{{color:'#1e6ee6',weight:2.5}}).addTo(m);
    for(var i=1;i<c.length-1;i++){{
      L.circleMarker(c[i],{{color:'#ff8c00',fillColor:'#ff8c00',fillOpacity:1,radius:4,weight:1}}).addTo(m);
    }}
    L.circleMarker(c[0],{{color:'#22cc44',fillColor:'#22cc44',fillOpacity:1,radius:6,weight:2}}).addTo(m);
    if(c.length>1)L.circleMarker(c[c.length-1],{{color:'#dd3333',fillColor:'#dd3333',fillOpacity:1,radius:6,weight:2}}).addTo(m);
    m.fitBounds(pl.getBounds(),{{padding:[20,20]}});
  }}
}})();
"#
                ));
            }
        }

        html.push_str("</div>\n"); // close .wrap
        html.push_str(&format!("<script>\n{map_scripts}</script>\n"));
        html.push_str("</body>\n</html>");

        if let Some(path) = rfd::FileDialog::new()
            .add_filter("HTML Report", &["html"])
            .set_file_name("gps_report.html")
            .save_file()
        {
            match std::fs::write(&path, &html) {
                Ok(()) => self.status = format!("Report saved → {}", path.display()),
                Err(e) => self.status = format!("Failed to save report: {e}"),
            }
        }
    }

    // ── PDF report ────────────────────────────────────────────────────────────

    fn build_pdf_report(&mut self, ids: Vec<String>) {
        use printpdf::*;

        if ids.is_empty() {
            return;
        }

        let result: Result<Vec<u8>, String> = (|| {
            let mm = |v: f64| Mm(v as f32);

            let (doc, pg0, ly0) =
                PdfDocument::new("GPS Log Report", mm(210.0), mm(297.0), "Main");
            let font_reg = doc.add_builtin_font(BuiltinFont::Helvetica).map_err(|e| e.to_string())?;
            let font_bold = doc.add_builtin_font(BuiltinFont::HelveticaBold).map_err(|e| e.to_string())?;

            let col_dark   = Color::Rgb(Rgb::new(0.10, 0.165, 0.29, None));
            let col_gray   = Color::Rgb(Rgb::new(0.50, 0.50,  0.50, None));
            let col_black  = Color::Rgb(Rgb::new(0.0,  0.0,   0.0,  None));
            let col_bg     = Color::Rgb(Rgb::new(0.86, 0.90,  0.95, None));
            let col_border = Color::Rgb(Rgb::new(0.45, 0.55,  0.70, None));
            let col_blue   = Color::Rgb(Rgb::new(0.12, 0.43,  0.90, None));
            let col_green  = Color::Rgb(Rgb::new(0.13, 0.80,  0.27, None));
            let col_orange = Color::Rgb(Rgb::new(1.00, 0.55,  0.00, None));
            let col_red    = Color::Rgb(Rgb::new(0.87, 0.20,  0.20, None));

            // ── Summary page ──────────────────────────────────────────────────
            {
                let lyr = doc.get_page(pg0).get_layer(ly0);
                lyr.set_fill_color(col_dark.clone());
                lyr.use_text("GPS Log Report", 20.0, mm(15.0), mm(275.0), &font_bold);

                let total: usize = ids
                    .iter()
                    .filter_map(|id| self.records.get(id))
                    .map(|v| v.len())
                    .sum();
                lyr.set_fill_color(col_gray.clone());
                lyr.use_text(
                    &format!("{} source IDs  |  {} total fixes (after dedup)", ids.len(), total),
                    9.0, mm(15.0), mm(268.0), &font_reg,
                );

                pdf_hline(&lyr, 15.0, 265.5, 195.0, col_border.clone());

                let cols = [15.0_f64, 55.0, 72.0, 100.0, 140.0, 172.0];
                let hdrs = ["Source ID", "Fixes", "Jumps", "First Fix", "Last Fix", "Last Position"];
                let mut y = 261.0_f64;
                lyr.set_fill_color(col_dark.clone());
                for (&x, h) in cols.iter().zip(hdrs.iter()) {
                    lyr.use_text(*h, 8.0, mm(x), mm(y), &font_bold);
                }
                y -= 5.5;

                for id in &ids {
                    if y < 20.0 {
                        break;
                    }
                    if let Some(recs) = self.records.get(id) {
                        let jumps = self.jump_counts.get(id).copied().unwrap_or(0);
                        let first_ts = recs.first().map(|r| r.timestamp.as_str()).unwrap_or("-");
                        let last_ts  = recs.last() .map(|r| r.timestamp.as_str()).unwrap_or("-");
                        let last_lat = recs.last().map(|r| r.lat).unwrap_or(0.0);
                        let last_lon = recs.last().map(|r| r.lon).unwrap_or(0.0);
                        fn trunc(s: &str) -> &str { if s.len() > 18 { &s[..18] } else { s } }

                        lyr.set_fill_color(col_black.clone());
                        lyr.use_text(id,                      8.0, mm(cols[0]), mm(y), &font_reg);
                        lyr.use_text(&recs.len().to_string(), 8.0, mm(cols[1]), mm(y), &font_reg);

                        let jcol = match jumps {
                            0      => Color::Rgb(Rgb::new(0.13, 0.68, 0.38, None)),
                            1..=15 => Color::Rgb(Rgb::new(0.79, 0.49, 0.06, None)),
                            _      => Color::Rgb(Rgb::new(0.75, 0.22, 0.17, None)),
                        };
                        lyr.set_fill_color(jcol);
                        lyr.use_text(&jumps.to_string(), 8.0, mm(cols[2]), mm(y), &font_bold);

                        lyr.set_fill_color(col_black.clone());
                        lyr.use_text(trunc(first_ts), 7.0, mm(cols[3]), mm(y), &font_reg);
                        lyr.use_text(trunc(last_ts),  7.0, mm(cols[4]), mm(y), &font_reg);
                        lyr.use_text(
                            &format!("{:.4}, {:.4}", last_lat, last_lon),
                            7.0, mm(cols[5]), mm(y), &font_reg,
                        );
                        y -= 5.5;
                    }
                }
            }

            // ── Per-ID pages ──────────────────────────────────────────────────
            for id in &ids {
                if let Some(recs) = self.records.get(id) {
                    let jumps    = self.jump_counts.get(id).copied().unwrap_or(0);
                    let first_ts = recs.first().map(|r| r.timestamp.as_str()).unwrap_or("-");
                    let last_ts  = recs.last() .map(|r| r.timestamp.as_str()).unwrap_or("-");
                    let last_lat = recs.last().map(|r| r.lat).unwrap_or(0.0);
                    let last_lon = recs.last().map(|r| r.lon).unwrap_or(0.0);

                    let (new_pg, new_ly) = doc.add_page(mm(210.0), mm(297.0), "Main");
                    let lyr = doc.get_page(new_pg).get_layer(new_ly);

                    // Header
                    lyr.set_fill_color(col_dark.clone());
                    lyr.use_text(
                        &format!("Source ID: {id}"), 16.0, mm(15.0), mm(275.0), &font_bold,
                    );
                    let (j_col, j_label) = match jumps {
                        0      => (Color::Rgb(Rgb::new(0.13, 0.68, 0.38, None)), "No large jumps".to_string()),
                        1..=15 => (Color::Rgb(Rgb::new(0.79, 0.49, 0.06, None)), format!("{jumps} large jump(s)")),
                        _      => (Color::Rgb(Rgb::new(0.75, 0.22, 0.17, None)), format!("{jumps} large jumps")),
                    };
                    lyr.set_fill_color(j_col);
                    lyr.use_text(&j_label, 9.0, mm(15.0), mm(268.5), &font_bold);
                    pdf_hline(&lyr, 15.0, 265.5, 195.0, col_border.clone());

                    // Stats table (left column)
                    let stats = [
                        ("Total fixes:",    recs.len().to_string()),
                        ("Large jumps:",    jumps.to_string()),
                        ("First fix:",      first_ts.to_string()),
                        ("Last fix:",       last_ts.to_string()),
                        ("Last latitude:",  format!("{:.6}", last_lat)),
                        ("Last longitude:", format!("{:.6}", last_lon)),
                    ];
                    let mut sy = 260.0_f64;
                    for (label, value) in &stats {
                        lyr.set_fill_color(Color::Rgb(Rgb::new(0.4, 0.4, 0.4, None)));
                        lyr.use_text(*label, 9.0, mm(15.0), mm(sy), &font_bold);
                        lyr.set_fill_color(col_black.clone());
                        lyr.use_text(value.as_str(), 9.0, mm(68.0), mm(sy), &font_reg);
                        sy -= 8.0;
                    }

                    // Breadcrumb map box (right side, 112–195 mm)
                    let mx = 112.0_f64;
                    let mw = 83.0_f64;
                    let mt = 263.0_f64;
                    let mh = 108.0_f64;
                    let mb = mt - mh;

                    lyr.set_fill_color(col_bg.clone());
                    lyr.set_outline_color(col_border.clone());
                    lyr.set_outline_thickness(0.8);
                    pdf_rect_filled(&lyr, mx, mb, mw, mh);

                    if !recs.is_empty() {
                        let pad = 5.0_f64;
                        let dw = mw - pad * 2.0;
                        let dh = mh - pad * 2.0;

                        let min_lat = recs.iter().map(|r| r.lat).fold(f64::INFINITY,     f64::min);
                        let max_lat = recs.iter().map(|r| r.lat).fold(f64::NEG_INFINITY, f64::max);
                        let min_lon = recs.iter().map(|r| r.lon).fold(f64::INFINITY,     f64::min);
                        let max_lon = recs.iter().map(|r| r.lon).fold(f64::NEG_INFINITY, f64::max);
                        let lat_s = (max_lat - min_lat).max(1e-5);
                        let lon_s = (max_lon - min_lon).max(1e-5);
                        let sc = (dw / lon_s).min(dh / lat_s);
                        let ox = mx + pad + (dw - lon_s * sc) / 2.0;
                        let oy = mb + pad + (dh - lat_s * sc) / 2.0;
                        // PDF Y increases upward → north is up naturally
                        let ppx = |lon: f64| ox + (lon - min_lon) * sc;
                        let ppy = |lat: f64| oy + (lat - min_lat) * sc;

                        if recs.len() > 1 {
                            lyr.set_outline_color(col_blue.clone());
                            lyr.set_outline_thickness(0.4);
                            let pts: Vec<(Point, bool)> = recs
                                .iter()
                                .map(|r| (Point::new(mm(ppx(r.lon)), mm(ppy(r.lat))), false))
                                .collect();
                            lyr.add_line(Line { points: pts, is_closed: false });
                        }

                        // Intermediate dots – all points except first and last
                        for (i, r) in recs.iter().enumerate() {
                            if i == 0 || i == recs.len() - 1 {
                                continue;
                            }
                            pdf_dot(&lyr, ppx(r.lon), ppy(r.lat), 1.0, col_orange.clone());
                        }
                        if let Some(r) = recs.first() {
                            pdf_dot(&lyr, ppx(r.lon), ppy(r.lat), 1.5, col_green.clone());
                        }
                        if recs.len() > 1 {
                            if let Some(r) = recs.last() {
                                pdf_dot(&lyr, ppx(r.lon), ppy(r.lat), 1.5, col_red.clone());
                            }
                        }

                        // Legend below map box
                        let leg_y = mb - 5.0;
                        pdf_dot(&lyr, mx + 2.5,  leg_y, 1.2, col_green.clone());
                        lyr.set_fill_color(col_gray.clone());
                        lyr.use_text("First fix",    6.5, mm(mx + 5.5),  mm(leg_y - 1.2), &font_reg);
                        pdf_dot(&lyr, mx + 30.0, leg_y, 1.2, col_red.clone());
                        lyr.use_text("Latest fix",   6.5, mm(mx + 33.0), mm(leg_y - 1.2), &font_reg);
                        pdf_dot(&lyr, mx + 62.0, leg_y, 0.9, col_orange.clone());
                        lyr.use_text("Intermediate", 6.5, mm(mx + 64.5), mm(leg_y - 1.2), &font_reg);
                    }
                }
            }

            doc.save_to_bytes().map_err(|e| e.to_string())
        })();

        match result {
            Err(e) => self.status = format!("PDF build error: {e}"),
            Ok(bytes) => {
                if let Some(path) = rfd::FileDialog::new()
                    .add_filter("PDF", &["pdf"])
                    .set_file_name("gps_report.pdf")
                    .save_file()
                {
                    match std::fs::write(&path, &bytes) {
                        Ok(()) => self.status = format!("PDF saved → {}", path.display()),
                        Err(e) => self.status = format!("Failed to save PDF: {e}"),
                    }
                }
            }
        }
    }

    fn open_map(&mut self, id: &str) {
        let entry = self.maps.entry(id.to_string()).or_insert_with(|| {
            let center = self
                .records
                .get(id)
                .and_then(|r| r.last())
                .map(|r| Position::from_lat_lon(r.lat, r.lon))
                .unwrap_or(Position::from_lat_lon(29.5, -82.8));
            MapWin { memory: MapMemory::default(), center, open: true }
        });
        entry.open = true;
    }
}

// ── PDF drawing helpers ───────────────────────────────────────────────────────

fn pdf_hline(
    lyr: &printpdf::PdfLayerReference,
    x1: f64, y: f64, x2: f64,
    color: printpdf::Color,
) {
    use printpdf::*;
    lyr.set_outline_color(color);
    lyr.set_outline_thickness(0.4);
    lyr.add_line(Line {
        points: vec![
            (Point::new(Mm(x1 as f32), Mm(y as f32)), false),
            (Point::new(Mm(x2 as f32), Mm(y as f32)), false),
        ],
        is_closed: false,
    });
}

fn pdf_rect_filled(
    lyr: &printpdf::PdfLayerReference,
    x: f64, y: f64, w: f64, h: f64,
) {
    use printpdf::*;
    lyr.add_polygon(Polygon {
        rings: vec![vec![
            (Point::new(Mm(x as f32),         Mm(y as f32)),         false),
            (Point::new(Mm((x + w) as f32),   Mm(y as f32)),         false),
            (Point::new(Mm((x + w) as f32),   Mm((y + h) as f32)),   false),
            (Point::new(Mm(x as f32),         Mm((y + h) as f32)),   false),
        ]],
        mode: PolygonMode::FillStroke,
        winding_order: WindingOrder::NonZero,
    });
}

/// Filled circle approximated as a 12-segment polygon.
fn pdf_dot(
    lyr: &printpdf::PdfLayerReference,
    cx: f64, cy: f64, r: f64,
    color: printpdf::Color,
) {
    use printpdf::*;
    lyr.set_fill_color(color);
    let n = 12_usize;
    let pts: Vec<(Point, bool)> = (0..=n)
        .map(|i| {
            let a = i as f64 * std::f64::consts::TAU / n as f64;
            (
                Point::new(
                    Mm((cx + r * a.cos()) as f32),
                    Mm((cy + r * a.sin()) as f32),
                ),
                false,
            )
        })
        .collect();
    lyr.add_polygon(Polygon {
        rings: vec![pts],
        mode: PolygonMode::Fill,
        winding_order: WindingOrder::NonZero,
    });
}

// ── Pre-extracted stats (avoids borrow conflicts in egui closures) ─────────────

struct IdStats {
    count: usize,
    first_ts: String,
    last_ts: String,
    last_lat: f64,
    last_lon: f64,
}

// ── egui update loop ──────────────────────────────────────────────────────────

impl App for GpsApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut Frame) {
        // ── Poll background load thread ───────────────────────────────────────
        let load_result: Option<Result<LoadResult, String>> =
            self.load_rx.as_ref().and_then(|rx| rx.try_recv().ok());

        match load_result {
            Some(Ok(result)) => {
                self.apply_load_result(result);
                self.is_loading = false;
                self.load_rx = None;
            }
            Some(Err(e)) => {
                self.status = e;
                self.is_loading = false;
                self.load_rx = None;
            }
            None => {}
        }

        // ── Top bar ───────────────────────────────────────────────────────────
        egui::TopBottomPanel::top("topbar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                let open_btn = ui.add_enabled(
                    !self.is_loading,
                    egui::Button::new("📂  Open Log File…"),
                );
                if open_btn.clicked() {
                    if let Some(p) = rfd::FileDialog::new()
                        .add_filter("GPS files", &["log", "txt", "slk"])
                        .pick_file()
                    {
                        self.start_load(p.display().to_string(), ctx.clone());
                    }
                }

                ui.separator();
                let has_data = !self.is_loading && !self.sorted_ids.is_empty();
                let has_sel  = !self.is_loading && !self.selection.is_empty();

                if ui.add_enabled(has_data, egui::Button::new("🌐 HTML – All")).clicked() {
                    let ids = self.sorted_ids.clone();
                    self.build_html_report(ids);
                }
                if ui.add_enabled(has_sel, egui::Button::new("🌐 HTML – Sel")).clicked() {
                    let ids = self.selected_ids_sorted();
                    self.build_html_report(ids);
                }

                ui.separator();

                if ui.add_enabled(has_data, egui::Button::new("📄 PDF – All")).clicked() {
                    let ids = self.sorted_ids.clone();
                    self.build_pdf_report(ids);
                }
                if ui.add_enabled(has_sel, egui::Button::new("📄 PDF – Sel")).clicked() {
                    let ids = self.selected_ids_sorted();
                    self.build_pdf_report(ids);
                }

                ui.separator();
                if self.is_loading {
                    ui.spinner();
                }
                ui.label(&self.status);
            });
        });

        // ── Left panel – Source IDs ───────────────────────────────────────────
        egui::SidePanel::left("id_panel")
            .min_width(220.0)
            .max_width(320.0)
            .default_width(240.0)
            .show(ctx, |ui| {
                ui.add_space(6.0);
                ui.heading("Source IDs");
                ui.add_space(4.0);

                egui::Frame::none()
                    .inner_margin(egui::Margin::symmetric(4.0, 3.0))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.colored_label(jump_color(0),  "●");
                            ui.label(egui::RichText::new("Clean").small());
                            ui.add_space(4.0);
                            ui.colored_label(jump_color(1),  "●");
                            ui.label(egui::RichText::new("1–15").small());
                            ui.add_space(4.0);
                            ui.colored_label(jump_color(16), "●");
                            ui.label(egui::RichText::new(">15").small());
                        });
                    });

                ui.separator();

                ui.horizontal(|ui| {
                    ui.label("🔍");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.search)
                            .hint_text("Filter IDs…")
                            .desired_width(150.0),
                    );
                    if ui.small_button("✕").clicked() {
                        self.search.clear();
                    }
                });
                ui.add_space(3.0);

                let q = self.search.trim().to_ascii_lowercase();
                let filtered: Vec<String> = self
                    .sorted_ids
                    .iter()
                    .filter(|id| q.is_empty() || id.to_ascii_lowercase().contains(&q))
                    .cloned()
                    .collect();

                let sel_count = self.selection.len();
                let shown_str = format!("{} / {} shown", filtered.len(), self.sorted_ids.len());
                let sel_str = if sel_count > 0 {
                    format!("  ·  {sel_count} selected")
                } else {
                    String::new()
                };
                ui.label(
                    egui::RichText::new(format!("{shown_str}{sel_str}"))
                        .small()
                        .color(Color32::GRAY),
                );
                ui.label(
                    egui::RichText::new("Ctrl+Click to multi-select")
                        .small()
                        .color(Color32::from_gray(140)),
                );
                ui.separator();

                let ctrl_held = ui.input(|i| i.modifiers.ctrl);

                ScrollArea::vertical().show(ui, |ui| {
                    for id in &filtered {
                        let count    = self.records.get(id).map(|v| v.len()).unwrap_or(0);
                        let jumps    = self.jump_counts.get(id).copied().unwrap_or(0);
                        let color    = jump_color(jumps);
                        let in_sel   = self.selection.contains(id);

                        ui.horizontal(|ui| {
                            ui.colored_label(color, "●");
                            let w = ui.available_width();
                            if ui
                                .add_sized(
                                    [w, 0.0],
                                    egui::SelectableLabel::new(
                                        in_sel,
                                        format!("{id}  ({count})"),
                                    ),
                                )
                                .clicked()
                            {
                                if ctrl_held {
                                    if self.selection.contains(id) {
                                        self.selection.remove(id);
                                    } else {
                                        self.selection.insert(id.clone());
                                    }
                                } else {
                                    self.selection.clear();
                                    self.selection.insert(id.clone());
                                }
                                self.focused = Some(id.clone());
                            }
                        });
                    }
                });
            });

        // ── Central panel ─────────────────────────────────────────────────────
        let focused_id = self.focused.clone();

        let stats: Option<IdStats> = focused_id.as_deref().and_then(|id| {
            self.records.get(id).map(|recs| IdStats {
                count:    recs.len(),
                first_ts: recs.first().map(|r| r.timestamp.clone()).unwrap_or_default(),
                last_ts:  recs.last() .map(|r| r.timestamp.clone()).unwrap_or_default(),
                last_lat: recs.last().map(|r| r.lat).unwrap_or(0.0),
                last_lon: recs.last().map(|r| r.lon).unwrap_or(0.0),
            })
        });

        let mut open_map_request: Option<String> = None;

        egui::CentralPanel::default().show(ctx, |ui| {
            match focused_id.as_deref() {
                None => {
                    ui.centered_and_justified(|ui| {
                        ui.label("← Select a source ID to view details.");
                    });
                }
                Some(id) => {
                    if let Some(ref s) = stats {
                        let jumps = self.jump_counts.get(id).copied().unwrap_or(0);
                        let jcolor = jump_color(jumps);
                        let jump_label = match jumps {
                            0 => "No large jumps".to_string(),
                            1 => "1 large jump".to_string(),
                            n => format!("{n} large jumps"),
                        };

                        ui.horizontal(|ui| {
                            ui.heading(format!("Source ID: {id}"));
                            ui.add_space(10.0);
                            ui.colored_label(jcolor, format!("●  {jump_label}"));
                        });
                        ui.separator();

                        egui::Grid::new("detail_grid")
                            .num_columns(2)
                            .spacing([16.0, 4.0])
                            .show(ui, |ui| {
                                ui.strong("Total fixes (after dedup):");
                                ui.label(s.count.to_string());
                                ui.end_row();
                                ui.strong("First timestamp:");
                                ui.label(&s.first_ts);
                                ui.end_row();
                                ui.strong("Latest timestamp:");
                                ui.label(&s.last_ts);
                                ui.end_row();
                                ui.strong("Latest latitude:");
                                ui.label(format!("{:.6}", s.last_lat));
                                ui.end_row();
                                ui.strong("Latest longitude:");
                                ui.label(format!("{:.6}", s.last_lon));
                                ui.end_row();
                            });

                        ui.add_space(10.0);
                        if ui.button("📍  Open Breadcrumb Map").clicked() {
                            open_map_request = Some(id.to_string());
                        }

                        ui.add_space(12.0);
                        ui.separator();
                        ui.strong("Fix History (deduplicated)");
                        ui.add_space(4.0);

                        if let Some(recs) = self.records.get(id) {
                            TableBuilder::new(ui)
                                .striped(true)
                                .column(Column::exact(44.0))
                                .column(Column::exact(175.0))
                                .column(Column::exact(100.0))
                                .column(Column::remainder())
                                .header(20.0, |mut h| {
                                    h.col(|ui| { ui.strong("#"); });
                                    h.col(|ui| { ui.strong("Timestamp"); });
                                    h.col(|ui| { ui.strong("Latitude"); });
                                    h.col(|ui| { ui.strong("Longitude"); });
                                })
                                .body(|body| {
                                    body.rows(18.0, recs.len(), |mut row| {
                                        let i = row.index();
                                        let r = &recs[i];
                                        row.col(|ui| {
                                            ui.label(
                                                egui::RichText::new(format!("{}", i + 1))
                                                    .color(Color32::GRAY),
                                            );
                                        });
                                        row.col(|ui| { ui.label(&r.timestamp); });
                                        row.col(|ui| { ui.label(format!("{:.6}", r.lat)); });
                                        row.col(|ui| { ui.label(format!("{:.6}", r.lon)); });
                                    });
                                });
                        }
                    }
                }
            }
        });

        if let Some(id) = open_map_request {
            self.open_map(&id);
        }

        // ── Map pop-out windows ───────────────────────────────────────────────
        let open_ids: Vec<String> = self
            .maps
            .iter()
            .filter(|(_, w)| w.open)
            .map(|(id, _)| id.clone())
            .collect();

        let tiles: &mut HttpTiles = self.tiles.as_mut().unwrap();
        let maps    = &mut self.maps;
        let records = &self.records;

        for id in &open_ids {
            let pts: Vec<(f64, f64)> = records
                .get(id)
                .map(|recs| recs.iter().map(|r| (r.lat, r.lon)).collect())
                .unwrap_or_default();

            let map_win = maps.get_mut(id).unwrap();
            let center  = map_win.center;
            let mut still_open = true;

            egui::Window::new(format!("Map – Source {id}"))
                .open(&mut still_open)
                .default_size([680.0, 500.0])
                .min_size([400.0, 300.0])
                .resizable(true)
                .show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        ui.colored_label(Color32::GREEN, "●");
                        ui.label("First fix");
                        ui.separator();
                        ui.colored_label(Color32::from_rgb(255, 140, 0), "●");
                        ui.label("Intermediate");
                        ui.separator();
                        ui.colored_label(Color32::RED, "●");
                        ui.label("Latest fix");
                        ui.separator();
                        ui.label(format!("{} fixes", pts.len()));
                    });
                    ui.separator();

                    let avail = ui.available_size();
                    ui.add_sized(
                        avail,
                        Map::new(
                            Some(&mut *tiles as &mut dyn Tiles),
                            &mut map_win.memory,
                            center,
                        )
                        .with_plugin(Breadcrumbs { points: pts }),
                    );
                });

            map_win.open = still_open;
        }
    }
}

// ── Entry point ───────────────────────────────────────────────────────────────

fn main() {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("Failed to create Tokio runtime");
    let _guard = rt.enter();

    eframe::run_native(
        "GPS Log Viewer",
        eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default()
                .with_inner_size([1200.0, 740.0])
                .with_title("GPS Log Viewer"),
            ..Default::default()
        },
        Box::new(|cc| Ok(Box::new(GpsApp::new(cc)))),
    )
    .expect("eframe failed");
}
