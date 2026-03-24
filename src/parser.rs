use rayon::prelude::*;
use regex::Regex;
use std::collections::HashMap;
use std::sync::OnceLock;

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
