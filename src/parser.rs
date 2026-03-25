use rayon::prelude::*;
use regex::Regex;
use std::collections::HashMap;
use std::sync::OnceLock;

// ── SYLK (.slk) parser ────────────────────────────────────────────────────────

/// Parse a SYLK (.slk) spreadsheet and return GPS records grouped by vehicle name.
///
/// Expected columns (detected from header row Y=1):
///   VehicleName, EventTime (Excel serial date), Lat, Lon
pub fn parse_slk(content: &str) -> (HashMap<String, Vec<GpsRecord>>, ParseStats) {
    // ── Phase 1: build sparse grid ────────────────────────────────────────────
    let mut grid: HashMap<u32, HashMap<u32, String>> = HashMap::new();
    let mut cur_row: u32 = 0;
    let mut cur_col: u32 = 0;

    for raw_line in content.lines() {
        let line = raw_line.trim_end_matches('\r');
        if line.starts_with("F;") {
            for part in line.split(';') {
                if let Some(y) = part.strip_prefix('Y') {
                    if let Ok(n) = y.parse::<u32>() { cur_row = n; }
                }
                if let Some(x) = part.strip_prefix('X') {
                    if let Ok(n) = x.parse::<u32>() { cur_col = n; }
                }
            }
        } else if line.starts_with("C;") {
            for part in line[2..].split(';') {
                if let Some(k) = part.strip_prefix('K') {
                    let value = if k.starts_with('"') {
                        k.trim_matches('"').to_string()
                    } else {
                        k.to_string()
                    };
                    grid.entry(cur_row).or_default().insert(cur_col, value);
                    break;
                }
            }
        }
    }

    // ── Phase 2: detect column indices from header row (Y=1) ──────────────────
    let empty = HashMap::new();
    let headers = grid.get(&1).unwrap_or(&empty);

    let find_col = |name: &str| -> Option<u32> {
        headers.iter()
            .find(|(_, v)| v.eq_ignore_ascii_case(name))
            .map(|(k, _)| *k)
    };

    let col_vehicle = find_col("VehicleName").unwrap_or(5);
    let col_time    = find_col("EventTime").unwrap_or(6);
    let col_lat     = find_col("Lat").unwrap_or(10);
    let col_lon     = find_col("Lon").unwrap_or(11);

    // ── Phase 3: extract GPS records from data rows (Y >= 2) ──────────────────
    let mut raw_fixes = 0usize;
    let mut map: HashMap<String, Vec<GpsRecord>> = HashMap::new();

    let mut rows: Vec<u32> = grid.keys().filter(|&&r| r >= 2).cloned().collect();
    rows.sort_unstable();

    for row in rows {
        let cols = &grid[&row];
        let vehicle  = cols.get(&col_vehicle).cloned().unwrap_or_default();
        let time_str = cols.get(&col_time).map(String::as_str).unwrap_or("");
        let lat_str  = cols.get(&col_lat).map(String::as_str).unwrap_or("");
        let lon_str  = cols.get(&col_lon).map(String::as_str).unwrap_or("");

        if vehicle.is_empty() || time_str.is_empty() { continue; }
        let lat: f64 = match lat_str.parse() { Ok(v) => v, Err(_) => continue };
        let lon: f64 = match lon_str.parse() { Ok(v) => v, Err(_) => continue };

        let timestamp = excel_serial_to_timestamp(time_str);
        raw_fixes += 1;
        map.entry(vehicle.clone()).or_default().push(GpsRecord {
            timestamp,
            source_id: vehicle,
            lat,
            lon,
        });
    }

    // ── Phase 4: dedup consecutive identical positions ─────────────────────────
    map.par_iter_mut().for_each(|(_, records)| dedup_consecutive(records));
    let after_dedup: usize = map.values().map(|v| v.len()).sum();

    (map, ParseStats { raw_fixes, after_dedup })
}

/// Convert an Excel OA serial date (float string) to "MM/DD/YYYY HH:MM:SS".
///
/// Excel OA epoch = Dec 30, 1899.  For all modern dates (serial > 60) the
/// JDN of the epoch is 2415019, so JDN = serial + 2415019.
fn excel_serial_to_timestamp(s: &str) -> String {
    let serial: f64 = match s.parse() {
        Ok(v) => v,
        Err(_) => return s.to_string(),
    };

    let int_days = serial as i64;
    let frac     = serial - int_days as f64;

    let total_secs = (frac * 86400.0).round() as u64;
    let hh = total_secs / 3600;
    let mi = (total_secs % 3600) / 60;
    let ss = total_secs % 60;

    let jdn = int_days + 2415019;
    let (year, month, day) = jdn_to_gregorian(jdn);

    format!("{:02}/{:02}/{} {:02}:{:02}:{:02}", month, day, year, hh, mi, ss)
}

/// Richards' algorithm: Julian Day Number → Gregorian (year, month, day).
fn jdn_to_gregorian(jdn: i64) -> (i32, u32, u32) {
    let a = jdn + 32044;
    let b = (4 * a + 3) / 146097;
    let c = a - (146097 * b) / 4;
    let d = (4 * c + 3) / 1461;
    let e = c - (1461 * d) / 4;
    let m = (5 * e + 2) / 153;
    let day   = (e - (153 * m + 2) / 5 + 1) as u32;
    let month = (m + 3 - 12 * (m / 10)) as u32;
    let year  = (100 * b + d - 4800 + m / 10) as i32;
    (year, month, day)
}

/// A single GPS fix from a `rxLocationReport` log line.
#[derive(Clone, Debug)]
pub struct GpsRecord {
    pub timestamp: String,
    pub source_id: String,
    pub lat: f64,
    pub lon: f64,
}

/// Summary returned alongside the parsed records.
pub struct ParseStats {
    pub raw_fixes: usize,
    pub after_dedup: usize,
}

static RE: OnceLock<Regex> = OnceLock::new();

fn get_regex() -> &'static Regex {
    RE.get_or_init(|| {
        Regex::new(
            r"(\d{2}/\d{2}/\d{2} \d{2}:\d{2}:\d{2}\.\d{3}).*?rxLocationReport targetId: \(\) sourceId: \((\d+)\) latitude: \(([-\d.]+)\) longitude: \(([-\d.]+)\)"
        )
        .expect("regex must compile")
    })
}

/// Parse the log in parallel (rayon) and return deduplicated records + stats.
///
/// Deduplication removes consecutive fixes for the same source ID that share
/// identical coordinates (within 1e-5° ≈ 1 m) – i.e. the device hasn't moved.
pub fn parse_log(content: &str) -> (HashMap<String, Vec<GpsRecord>>, ParseStats) {
    let re = get_regex();

    // ── Phase 1: parallel regex match across all lines ────────────────────────
    // Collect lines first so rayon can index them.
    let lines: Vec<&str> = content.lines().collect();

    // Each matched line produces Some(GpsRecord), non-matching lines produce None.
    // par_iter preserves index order so results stay chronological.
    let matched: Vec<Option<GpsRecord>> = lines
        .par_iter()
        .map(|line| {
            re.captures(line).map(|caps| GpsRecord {
                timestamp: caps[1].to_string(),
                source_id: caps[2].to_string(),
                lat: caps[3].parse().unwrap_or(0.0),
                lon: caps[4].parse().unwrap_or(0.0),
            })
        })
        .collect();

    // ── Phase 2: group by source ID (sequential, but cheap) ──────────────────
    let mut map: HashMap<String, Vec<GpsRecord>> = HashMap::new();
    for record in matched.into_iter().flatten() {
        map.entry(record.source_id.clone()).or_default().push(record);
    }

    let raw_fixes: usize = map.values().map(|v| v.len()).sum();

    // ── Phase 3: per-ID deduplication in parallel ─────────────────────────────
    // Removes consecutive fixes where (lat, lon) hasn't changed.
    map.par_iter_mut().for_each(|(_, records)| {
        dedup_consecutive(records);
    });

    let after_dedup: usize = map.values().map(|v| v.len()).sum();

    (map, ParseStats { raw_fixes, after_dedup })
}

/// Remove consecutive entries with identical coordinates (within epsilon).
/// The first record is always kept; subsequent records are dropped only when
/// both lat and lon haven't changed from the previously kept record.
fn dedup_consecutive(records: &mut Vec<GpsRecord>) {
    const EPS: f64 = 1e-5; // ~1 metre
    let mut prev: Option<(f64, f64)> = None;
    records.retain(|r| {
        let keep = match prev {
            None => true, // always keep the first fix
            Some((plat, plon)) => {
                (r.lat - plat).abs() > EPS || (r.lon - plon).abs() > EPS
            }
        };
        if keep {
            prev = Some((r.lat, r.lon));
        }
        keep
    });
}
