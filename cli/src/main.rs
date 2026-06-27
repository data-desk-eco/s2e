//! s2-flares native cli — sentinel-2 swir flare detection (gdal-backed). two
//! subcommands, one frozen methodology core (shared with the wasm/browser path):
//!
//!   s2-flares detect  (--bbox|--aoi) --out DIR   grow the DETECTION archive: one
//!       csv per scene under DIR/<id>/<mgrs>_<date>.csv, file presence == done →
//!       resumable. this is the analytical source of truth (loose, recall-first).
//!
//!   s2-flares cluster (--archive GLOB | --bbox|--aoi) --out FILE   the derived
//!       cluster VIEW — run core clustering over archived (or freshly detected)
//!       detections; `.geojson` → rich FeatureCollection (journalist), else nested
//!       parquet (one row/cluster + a `detections` list; the web map column-skips
//!       it). computed separately from detection, written to a `clusters/` prefix.

mod stac;
mod read;
mod view;

use std::fs;
use std::path::Path;
use rayon::prelude::*;
use s2_flares_core::{cluster_detections, pad_bbox, ClusterOptions, Detection, Thresholds};

#[derive(PartialEq)]
enum Mode { Detect, Cluster }

struct Args {
    mode: Mode,
    bbox: Option<[f64; 4]>,
    aoi: Option<String>,
    archive: Option<String>,
    buffer: f64,
    start: String,
    end: String,
    max_cloud_cover: f64,
    preset: String,
    source: String,
    concurrency: usize,
    out: Option<String>,
    min_dates: usize,
    min_avg_b12: f64,
    score_threshold: f64,
}

struct Aoi { id: String, name: String, bbox: [f64; 4] }

fn die(msg: &str) -> ! { eprintln!("{msg}"); std::process::exit(1); }

fn parse_args() -> Args {
    let argv: Vec<String> = std::env::args().collect();
    let mode = match argv.get(1).map(String::as_str) {
        Some("detect") => Mode::Detect,
        Some("cluster") => Mode::Cluster,
        Some("--help") | Some("-h") | None => { println!("{USAGE}"); std::process::exit(0); }
        Some(x) => die(&format!("unknown subcommand '{x}' (expected detect|cluster)")),
    };
    // detect defaults to loose (recall-first archive); cluster's min_dates defaults to 1
    // (map-friendly — every scored site surfaces; gate downstream).
    let mut a = Args {
        bbox: None, aoi: None, archive: None, buffer: 0.0, start: String::new(), end: String::new(),
        max_cloud_cover: 100.0, preset: if mode == Mode::Detect { "loose".into() } else { "loose".into() },
        source: "aws".into(), concurrency: 4, out: None, min_dates: 1, min_avg_b12: 0.5, score_threshold: 0.0,
        mode,
    };
    let mut i = 2;
    while i < argv.len() {
        let k = argv[i].clone();
        let mut next = || { i += 1; argv.get(i).cloned().unwrap_or_else(|| die(&format!("missing value for {k}"))) };
        match k.as_str() {
            "--bbox" => { let v: Vec<f64> = next().split(',').map(|x| x.parse().unwrap()).collect();
                if v.len() != 4 { die("--bbox needs W,S,E,N"); } a.bbox = Some([v[0], v[1], v[2], v[3]]); }
            "--aoi" => a.aoi = Some(next()),
            "--archive" => a.archive = Some(next()),
            "--buffer" => a.buffer = next().parse().unwrap(),
            "--start" => a.start = next(),
            "--end" => a.end = next(),
            "--cloud" => a.max_cloud_cover = next().parse().unwrap(),
            "--preset" => a.preset = next(),
            "--source" => a.source = next(),
            "--concurrency" => a.concurrency = next().parse().unwrap(),
            "--out" => a.out = Some(next()),
            "--min-dates" => a.min_dates = next().parse().unwrap(),
            "--min-avg-b12" => a.min_avg_b12 = next().parse().unwrap(),
            "--score-threshold" => a.score_threshold = next().parse().unwrap(),
            "--help" | "-h" => { println!("{USAGE}"); std::process::exit(0); }
            _ => die(&format!("unknown argument: {k}")),
        }
        i += 1;
    }
    if a.mode == Mode::Detect && a.bbox.is_none() && a.aoi.is_none() { die("detect: provide --bbox or --aoi"); }
    if a.mode == Mode::Cluster && a.archive.is_none() && a.bbox.is_none() && a.aoi.is_none() {
        die("cluster: provide --archive GLOB, or --bbox/--aoi to detect fresh");
    }
    if a.preset != "default" && a.preset != "loose" { die("--preset must be 'default' or 'loose'"); }
    if a.start.is_empty() { a.start = days_ago(183); }
    if a.end.is_empty() { a.end = today(); }
    a
}

const USAGE: &str = "s2-flares — Sentinel-2 SWIR flare detection (native gdal)\n\n\
Usage:\n\
  s2-flares detect  (--bbox W,S,E,N | --aoi f.geojson) --out DIR   grow detection archive\n\
  s2-flares cluster (--archive GLOB | --bbox | --aoi) --out FILE   derive cluster view\n\n\
Options:\n\
  --buffer KM            halo around each aoi (default 0)\n\
  --start/--end Y-M-D    date window (default last ~6 months)\n\
  --cloud N              max scene cloud cover % (default 100)\n\
  --preset default|loose thresholds (default loose: recall-first archive)\n\
  --source aws|cdse      stac/reader profile (aws cog default; cdse eodata jp2)\n\
  --concurrency N        scenes in flight (default 4)\n\
  --archive GLOB         (cluster) detection source: duckdb-readable parquet/csv\n\
                         glob, e.g. s3://bkt/flares/preset=loose/**/*.parquet\n\
  --out PATH             detect: DIR. cluster: FILE (.geojson, or .parquet / s3://\n\
                         clusters/… → nested view); omit → geojson to stdout\n\
  --min-dates/--min-avg-b12/--score-threshold   cluster knobs";

// --- aoi loading -------------------------------------------------------------
fn geom_bbox(geom: &serde_json::Value) -> [f64; 4] {
    let (mut w, mut s, mut e, mut n) = (f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY);
    fn walk(c: &serde_json::Value, w: &mut f64, s: &mut f64, e: &mut f64, n: &mut f64) {
        if let Some(arr) = c.as_array() {
            if arr.first().and_then(|x| x.as_f64()).is_some() && arr.len() >= 2 {
                let (x, y) = (arr[0].as_f64().unwrap(), arr[1].as_f64().unwrap());
                *w = w.min(x); *e = e.max(x); *s = s.min(y); *n = n.max(y);
            } else {
                for x in arr { walk(x, w, s, e, n); }
            }
        }
    }
    walk(&geom["coordinates"], &mut w, &mut s, &mut e, &mut n);
    [w, s, e, n]
}

fn load_aois(a: &Args) -> Vec<Aoi> {
    if let Some(b) = a.bbox { return vec![Aoi { id: "aoi".into(), name: String::new(), bbox: b }]; }
    let text = fs::read_to_string(a.aoi.as_ref().unwrap()).unwrap_or_else(|e| die(&format!("read aoi: {e}")));
    let gj: serde_json::Value = serde_json::from_str(&text).unwrap_or_else(|e| die(&format!("parse aoi: {e}")));
    let feats = gj["features"].as_array().cloned().unwrap_or_default();
    feats.iter().enumerate().map(|(idx, f)| {
        let p = &f["properties"];
        let id = p["id"].as_str().map(String::from)
            .or_else(|| p["ProjectID"].as_str().map(String::from))
            .unwrap_or_else(|| idx.to_string());
        let name = p["name"].as_str().or_else(|| p["TerminalName"].as_str()).unwrap_or("").to_string();
        Aoi { id, name, bbox: pad_bbox(geom_bbox(&f["geometry"]), a.buffer) }
    }).collect()
}

fn thresholds(preset: &str) -> Thresholds {
    if preset == "loose" { Thresholds::loose() } else { Thresholds::defaults() }
}

fn fmt(x: f64) -> String {
    if x.is_infinite() { if x < 0.0 { "-Infinity".into() } else { "Infinity".into() } } else { format!("{x}") }
}
fn fmt_opt(x: Option<f64>) -> String { x.map(fmt).unwrap_or_default() }

// per-scene detection-archive csv — identical schema to lambda/handler.js & cf-run.js.
fn scene_row(d: &Detection) -> String {
    [
        fmt(d.lon), fmt(d.lat), d.date.clone(), d.mgrs.clone(), d.scene.clone(),
        fmt(d.max_b12), fmt(d.avg_b12), fmt_opt(d.peak_b11), fmt_opt(d.b12_b11_ratio),
        fmt(d.peakedness), d.pixels.to_string(), d.warm_size.to_string(), d.saturated.to_string(),
        fmt_opt(d.sun_elevation), fmt_opt(d.sun_azimuth), fmt_opt(d.glint_angle), fmt_opt(d.glint_score),
    ].join(",")
}
const SCENE_HEADER: &str = "lon,lat,date,mgrs,scene,max_b12,avg_b12,max_b11,b12_b11_ratio,peakedness,pixels,warm_size,saturated,sun_elevation,sun_azimuth,glint_angle,glint_score";

fn main() {
    let a = parse_args();
    read::configure();
    let pool = rayon::ThreadPoolBuilder::new().num_threads(a.concurrency.max(1)).build().unwrap();
    match a.mode {
        Mode::Detect => run_detect(&a, &pool),
        Mode::Cluster => run_cluster(&a, &pool),
    }
}

// grow the detection archive: one csv per scene, presence == done → resumable.
fn run_detect(a: &Args, pool: &rayon::ThreadPool) {
    let t = thresholds(&a.preset);
    let aois = load_aois(a);
    let out = a.out.clone().unwrap_or_else(|| "out".into());
    let harmonize = a.source != "aws"; // aws cogs pre-harmonised; eodata jp2 isn't
    eprintln!("detect: {} aoi(s) | {} → {} | preset={} | source={} → {out}/", aois.len(), a.start, a.end, a.preset, a.source);
    let (mut scenes, mut detected) = (0usize, 0usize);
    for aoi in &aois {
        let items = match stac::search(aoi.bbox, &a.start, &a.end, a.max_cloud_cover, &a.source) {
            Ok(v) => v, Err(e) => { eprintln!("  {} search FAIL: {e}", aoi.id); continue; }
        };
        scenes += items.len();
        let (done, skipped, det) = pool.install(|| {
            items.par_iter().map(|item| {
                let path = format!("{out}/{}/{}_{}.csv", aoi.id, item.mgrs, item.date);
                if Path::new(&path).exists() { return (0usize, 1usize, 0usize); }
                match read::detect_image(item, aoi.bbox, &t, false, harmonize) {
                    Ok((dets, _)) => {
                        let _ = fs::create_dir_all(Path::new(&path).parent().unwrap());
                        let body: String = std::iter::once(SCENE_HEADER.to_string())
                            .chain(dets.iter().map(scene_row)).collect::<Vec<_>>().join("\n") + "\n";
                        let _ = fs::write(&path, body);
                        eprintln!("  {} {}_{}: {} det", aoi.id, item.mgrs, item.date, dets.len());
                        (1, 0, dets.len())
                    }
                    Err(e) => { eprintln!("  {} {}_{} FAIL: {e}", aoi.id, item.mgrs, item.date); (0, 0, 0) }
                }
            }).reduce(|| (0, 0, 0), |x, y| (x.0 + y.0, x.1 + y.1, x.2 + y.2))
        });
        detected += det;
        eprintln!("  {} {}: {} scenes ({} new, {} cached), {} detections", aoi.id, aoi.name, items.len(), done, skipped, det);
    }
    eprintln!("\ndone: {scenes} scenes, {detected} detections → {out}/");
    eprintln!("archive to parquet, then: s2-flares cluster --archive '{out}/**/*.parquet' --out clusters.parquet");
}

// the derived cluster view — over the archive (--archive) or a fresh detect.
fn run_cluster(a: &Args, pool: &rayon::ThreadPool) {
    let (detections, observations) = match &a.archive {
        Some(glob) => {
            eprintln!("cluster: archive {glob} | {} → {}", a.start, a.end);
            (view::read_archive(glob, a.bbox, &a.start, &a.end).unwrap_or_else(|e| die(&e)), None)
        }
        None => {
            let t = thresholds(&a.preset);
            let aois = load_aois(a);
            let harmonize = a.source != "aws";
            eprintln!("cluster: fresh detect over {} aoi(s) | {} → {} | preset={}", aois.len(), a.start, a.end, a.preset);
            let mut dets = Vec::new();
            let mut obs: std::collections::HashMap<String, bool> = std::collections::HashMap::new();
            for aoi in &aois {
                let items = match stac::search(aoi.bbox, &a.start, &a.end, a.max_cloud_cover, &a.source) {
                    Ok(v) => v, Err(e) => { eprintln!("  {} search FAIL: {e}", aoi.id); continue; }
                };
                let res: Vec<(String, bool, Vec<Detection>)> = pool.install(|| items.par_iter().map(|item|
                    match read::detect_image(item, aoi.bbox, &t, true, harmonize) {
                        Ok((d, cf)) => (item.date.clone(), cf, d),
                        Err(e) => { eprintln!("  {} {}_{} FAIL: {e}", aoi.id, item.mgrs, item.date); (item.date.clone(), false, Vec::new()) }
                    }).collect());
                for (date, cf, d) in res { obs.insert(date, cf); dets.extend(d); }
                eprintln!("  {} {}: {} scenes", aoi.id, aoi.name, items.len());
            }
            (dets, Some(obs.values().filter(|v| **v).count()))
        }
    };

    let opts = ClusterOptions { merge_distance: 135.0, min_dates: a.min_dates, min_avg_b12: a.min_avg_b12,
        observations, score_threshold: a.score_threshold };
    let clusters = cluster_detections(&detections, &opts);
    eprintln!("{} detections → {} clusters", detections.len(), clusters.len());

    match &a.out {
        Some(path) => match view::write_view(&clusters, path) {
            Ok(()) => eprintln!("view → {path}"),
            Err(e) => die(&format!("write view: {e}")),
        },
        None => print!("{}", view::geojson(&clusters)),
    }
}

// --- minimal civil date helpers (avoid a chrono dependency) ------------------
fn epoch_days() -> i64 {
    (std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() / 86400) as i64
}
// days since 1970-01-01 → "YYYY-MM-DD" (Howard Hinnant's civil_from_days).
fn ymd(days: i64) -> String {
    let z = days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}
fn today() -> String { ymd(epoch_days()) }
fn days_ago(n: i64) -> String { ymd(epoch_days() - n) }
