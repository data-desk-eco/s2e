//! s2-flares native cli — sentinel-2 swir flare detection (gdal-backed). two
//! subcommands, one frozen methodology core (shared with the wasm/browser path):
//!
//!   s2-flares detect  (--bbox|--aoi) --out DIR   grow the DETECTION archive: one
//!       csv per scene under DIR/<id>/<mgrs>_<date>.csv, file presence == done →
//!       resumable. this is the analytical source of truth (recall-first defaults).
//!
//!   s2-flares cluster (--archive GLOB | --bbox|--aoi) --out FILE   the derived
//!       cluster VIEW — run core clustering over archived (or freshly detected)
//!       detections; `.geojson` → rich FeatureCollection (journalist), else nested
//!       parquet (one row/cluster + a `detections` list; the web map column-skips
//!       it). a derived view in its own `clusters/` prefix — on the box it's
//!       co-produced with the detections/ rollup; the web map re-clusters live in wasm.

mod stac;
mod read;
mod view;

use std::fs;
use std::path::Path;
use clap::{Args as ClapArgs, Parser, Subcommand};
use rayon::prelude::*;
use s2_flares_core::{cluster_detections, pad_bbox, ClusterOptions, Detection, Thresholds};

/// Sentinel-2 SWIR flare detection (native gdal).
#[derive(Parser)]
#[command(name = "s2-flares", version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Grow the detection archive — one csv per scene (presence == done → resumable).
    Detect {
        /// Output dir for the per-scene csv archive.
        #[arg(long, value_name = "DIR", default_value = "out")]
        out: String,
        // Common (with its knobs help-heading) goes last so the heading doesn't leak.
        #[command(flatten)]
        c: Common,
    },
    /// Derive the cluster view over the archive (--archive) or a fresh detect.
    Cluster {
        /// Detection source: a duckdb-readable parquet/csv glob, e.g.
        /// s3://bkt/detections/**/*.parquet (else --bbox/--aoi to detect fresh).
        #[arg(long, value_name = "GLOB")]
        archive: Option<String>,
        /// Output FILE (.geojson journalist · .parquet/s3://… nested web-map view);
        /// omit → geojson to stdout.
        #[arg(long, value_name = "FILE")]
        out: Option<String>,
        /// Min distinct dates per cluster.
        #[arg(long, default_value_t = 1)]
        min_dates: usize,
        /// Min mean B12 per cluster.
        #[arg(long, default_value_t = 0.5)]
        min_avg_b12: f64,
        /// Drop clusters scoring below this.
        #[arg(long, default_value_t = 0.0)]
        score_threshold: f64,
        #[command(flatten)]
        c: Common,
    },
}

/// options shared by both subcommands: the area, the search window, the reader
/// profile, and the recall-first detector knobs (every spectral gate a flag).
#[derive(ClapArgs)]
struct Common {
    /// Area of interest as West,South,East,North.
    #[arg(long, value_name = "W,S,E,N", value_parser = parse_bbox, allow_hyphen_values = true)]
    bbox: Option<[f64; 4]>,
    /// AOI geojson FeatureCollection (one run per feature).
    #[arg(long, value_name = "FILE")]
    aoi: Option<String>,
    /// Halo around each aoi, km.
    #[arg(long, value_name = "KM", default_value_t = 0.0)]
    buffer: f64,
    /// Window start (default ~6 months ago).
    #[arg(long, value_name = "Y-M-D")]
    start: Option<String>,
    /// Window end (default today).
    #[arg(long, value_name = "Y-M-D")]
    end: Option<String>,
    /// Max scene cloud cover %.
    #[arg(long, value_name = "PCT", default_value_t = 100.0)]
    cloud: f64,
    /// Reader profile: aws cog (default, no offset) or cdse eodata jp2 (harmonised).
    #[arg(long, default_value = "aws", value_parser = ["aws", "cdse"])]
    source: String,
    /// Scenes in flight.
    #[arg(long, default_value_t = 4)]
    concurrency: usize,
    #[command(flatten)]
    knobs: Knobs,
}

/// recall-first detector floors (the spectral mask always runs; these are the
/// tunable gates). raise any one to lean the archive — see Thresholds::default.
#[derive(ClapArgs)]
#[command(next_help_heading = "Detector knobs (recall-first; raise to tighten)")]
struct Knobs {
    /// B12 swir-hot reflectance floor.
    #[arg(long, default_value_t = 0.25)]
    b12_min: f64,
    /// B11 swir-hot reflectance floor.
    #[arg(long, default_value_t = 0.15)]
    b11_min: f64,
    /// Brightest-pixel B12 floor.
    #[arg(long, default_value_t = 0.30)]
    peak_b12_min: f64,
    /// Flare-vs-background contrast ratio.
    #[arg(long, default_value_t = 2.0)]
    contrast_ratio: f64,
    /// Background reflectance floor.
    #[arg(long, default_value_t = 0.10)]
    background_floor: f64,
    /// Spatial peakedness gate.
    #[arg(long, default_value_t = 1.0)]
    peakedness_min: f64,
}

impl Common {
    fn dates(&self) -> (String, String) {
        (self.start.clone().unwrap_or_else(|| days_ago(183)), self.end.clone().unwrap_or_else(today))
    }
    fn thresholds(&self) -> Thresholds {
        let k = &self.knobs;
        Thresholds {
            b12_min: k.b12_min, b11_min: k.b11_min, peak_b12_min: k.peak_b12_min,
            contrast_ratio: k.contrast_ratio, background_floor: k.background_floor,
            peakedness_min: k.peakedness_min, ..Default::default()
        }
    }
    fn harmonize(&self) -> bool { self.source != "aws" } // aws cogs pre-harmonised; eodata jp2 isn't
}

fn parse_bbox(s: &str) -> Result<[f64; 4], String> {
    let v: Vec<f64> = s.split(',').map(|x| x.trim().parse()).collect::<Result<_, _>>()
        .map_err(|e| format!("not a number: {e}"))?;
    v.try_into().map_err(|_| "expected W,S,E,N".into())
}

struct Aoi { id: String, name: String, bbox: [f64; 4] }

fn die(msg: &str) -> ! { eprintln!("{msg}"); std::process::exit(1); }

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

fn load_aois(c: &Common) -> Vec<Aoi> {
    if let Some(b) = c.bbox { return vec![Aoi { id: "aoi".into(), name: String::new(), bbox: b }]; }
    let text = fs::read_to_string(c.aoi.as_ref().unwrap()).unwrap_or_else(|e| die(&format!("read aoi: {e}")));
    let gj: serde_json::Value = serde_json::from_str(&text).unwrap_or_else(|e| die(&format!("parse aoi: {e}")));
    let feats = gj["features"].as_array().cloned().unwrap_or_default();
    feats.iter().enumerate().map(|(idx, f)| {
        let p = &f["properties"];
        let id = p["id"].as_str().map(String::from)
            .or_else(|| p["ProjectID"].as_str().map(String::from))
            .unwrap_or_else(|| idx.to_string());
        let name = p["name"].as_str().or_else(|| p["TerminalName"].as_str()).unwrap_or("").to_string();
        Aoi { id, name, bbox: pad_bbox(geom_bbox(&f["geometry"]), c.buffer) }
    }).collect()
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
    let cli = Cli::parse();
    read::configure();
    let pool = |n: usize| rayon::ThreadPoolBuilder::new().num_threads(n.max(1)).build().unwrap();
    match &cli.cmd {
        Cmd::Detect { c, out } => {
            if c.bbox.is_none() && c.aoi.is_none() { die("detect: provide --bbox or --aoi"); }
            run_detect(c, out, &pool(c.concurrency));
        }
        Cmd::Cluster { c, archive, out, min_dates, min_avg_b12, score_threshold } => {
            if archive.is_none() && c.bbox.is_none() && c.aoi.is_none() {
                die("cluster: provide --archive GLOB, or --bbox/--aoi to detect fresh");
            }
            run_cluster(c, archive, out, *min_dates, *min_avg_b12, *score_threshold, &pool(c.concurrency));
        }
    }
}

// grow the detection archive: one csv per scene, presence == done → resumable.
fn run_detect(c: &Common, out: &str, pool: &rayon::ThreadPool) {
    let t = c.thresholds();
    let aois = load_aois(c);
    let (start, end) = c.dates();
    let harmonize = c.harmonize();
    eprintln!("detect: {} aoi(s) | {start} → {end} | b12≥{} b11≥{} | source={} → {out}/", aois.len(), t.b12_min, t.b11_min, c.source);
    let (mut scenes, mut detected) = (0usize, 0usize);
    for aoi in &aois {
        let items = match stac::search(aoi.bbox, &start, &end, c.cloud, &c.source) {
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
fn run_cluster(c: &Common, archive: &Option<String>, out: &Option<String>,
    min_dates: usize, min_avg_b12: f64, score_threshold: f64, pool: &rayon::ThreadPool) {
    let (start, end) = c.dates();
    let (detections, observations) = match archive {
        Some(glob) => {
            eprintln!("cluster: archive {glob} | {start} → {end}");
            (view::read_archive(glob, c.bbox, &start, &end).unwrap_or_else(|e| die(&e)), None)
        }
        None => {
            let t = c.thresholds();
            let aois = load_aois(c);
            let harmonize = c.harmonize();
            eprintln!("cluster: fresh detect over {} aoi(s) | {start} → {end}", aois.len());
            let mut dets = Vec::new();
            let mut obs: std::collections::HashMap<String, bool> = std::collections::HashMap::new();
            for aoi in &aois {
                let items = match stac::search(aoi.bbox, &start, &end, c.cloud, &c.source) {
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

    let opts = ClusterOptions { merge_distance: 135.0, min_dates, min_avg_b12,
        observations, score_threshold };
    let clusters = cluster_detections(&detections, &opts);
    eprintln!("{} detections → {} clusters", detections.len(), clusters.len());

    match out {
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
