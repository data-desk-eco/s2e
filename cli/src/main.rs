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
#[cfg(feature = "gpu")]
mod gpu;

use std::fs;
use std::path::Path;
use clap::{Args as ClapArgs, Parser, Subcommand};
use rayon::prelude::*;
use s2_flares_core::{cluster_detections, pad_bbox, Cluster, ClusterOptions, Detection, Site, Thresholds};

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
        /// Min distinct dates per cluster (recall-first floor: drop true singletons only;
        /// rank on the score's clear-sky persistence term, don't hard-gate the count).
        #[arg(long, default_value_t = 2)]
        min_dates: usize,
        /// Min mean B12 per cluster.
        #[arg(long, default_value_t = 0.5)]
        min_avg_b12: f64,
        /// Drop clusters scoring below this.
        #[arg(long, default_value_t = 0.0)]
        score_threshold: f64,
        /// Cloud mask (clouds/ glob/parquet) for the persistence denominator: spatial-
        /// join each cluster anchor's ~100 m cell → n_clear_obs = distinct clear dates,
        /// rescoring the view. The fold-in default (one SCL read at detect, no 2nd pass).
        #[arg(long, value_name = "GLOB")]
        clouds: Option<String>,
        /// (validation/fallback) Site-anchored clear-sky coverage SCAN into DIR (resumable
        /// per-scene): re-read SCL at each anchor over every acquisition → persistence.
        /// Superseded by --clouds; kept to cross-check the fold-in. Needs a scene source.
        #[arg(long, value_name = "DIR")]
        coverage_scan: Option<String>,
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
    /// Wide-area: detect every MGRS tile intersecting this region over its WHOLE
    /// tile (not a window). The GPU reader's target — full-tile mapping, not points.
    #[arg(long, value_name = "W,S,E,N", value_parser = parse_bbox, allow_hyphen_values = true)]
    region: Option<[f64; 4]>,
    /// Restrict --region to these MGRS tiles (comma-separated, e.g. 39RWN,39RXN).
    #[arg(long, value_name = "MGRS,…", value_delimiter = ',')]
    tiles: Vec<String>,
    /// GPU-decode the bulk path (nvJPEG2000 batched full-tile) — use with --region; needs a --features gpu build.
    #[arg(long)]
    gpu: bool,
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
    /// Hot-core B12 floor: the `pixels`/`radiance` flare-size measurement counts
    /// only pixels above this (combustion-hot), not the loose detection mask.
    #[arg(long, default_value_t = 0.50)]
    hot_floor: f64,
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
            peakedness_min: k.peakedness_min, hot_floor: k.hot_floor, ..Default::default()
        }
    }
    fn harmonize(&self) -> bool { self.source != "aws" } // aws cogs pre-harmonised; eodata jp2 isn't
}

fn parse_bbox(s: &str) -> Result<[f64; 4], String> {
    let v: Vec<f64> = s.split(',').map(|x| x.trim().parse()).collect::<Result<_, _>>()
        .map_err(|e| format!("not a number: {e}"))?;
    v.try_into().map_err(|_| "expected W,S,E,N".into())
}

struct Aoi { id: String, name: String, bbox: [f64; 4], full_tile: bool }

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
    // --region: one wide-area job, scenes detected over their whole tile (full_tile).
    if let Some(b) = c.region { return vec![Aoi { id: "region".into(), name: String::new(), bbox: b, full_tile: true }]; }
    if let Some(b) = c.bbox { return vec![Aoi { id: "aoi".into(), name: String::new(), bbox: b, full_tile: false }]; }
    let text = fs::read_to_string(c.aoi.as_ref().unwrap()).unwrap_or_else(|e| die(&format!("read aoi: {e}")));
    let gj: serde_json::Value = serde_json::from_str(&text).unwrap_or_else(|e| die(&format!("parse aoi: {e}")));
    let feats = gj["features"].as_array().cloned().unwrap_or_default();
    feats.iter().enumerate().map(|(idx, f)| {
        let p = &f["properties"];
        let id = p["id"].as_str().map(String::from)
            .or_else(|| p["ProjectID"].as_str().map(String::from))
            .unwrap_or_else(|| idx.to_string());
        let name = p["name"].as_str().or_else(|| p["TerminalName"].as_str()).unwrap_or("").to_string();
        Aoi { id, name, bbox: pad_bbox(geom_bbox(&f["geometry"]), c.buffer), full_tile: false }
    }).collect()
}

// the per-scene detection region: a whole tile (full_tile/--region wide-area) or the
// query window. orthogonal to reader choice — the driver just passes this as `region`.
fn det_bbox(aoi: &Aoi, item: &stac::Item) -> [f64; 4] { if aoi.full_tile { item.bbox } else { aoi.bbox } }

// restrict a scene list to --tiles when given (a filter over the region search).
fn filter_tiles(c: &Common, items: &mut Vec<stac::Item>) {
    if !c.tiles.is_empty() { items.retain(|i| c.tiles.iter().any(|t| i.mgrs == *t)); }
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
        fmt(d.peakedness), d.pixels.to_string(), fmt(d.radiance), d.saturated.to_string(),
        fmt_opt(d.sun_elevation), fmt_opt(d.sun_azimuth), fmt_opt(d.glint_angle), fmt_opt(d.glint_score),
    ].join(",")
}
const SCENE_HEADER: &str = "lon,lat,date,mgrs,scene,max_b12,avg_b12,max_b11,b12_b11_ratio,peakedness,pixels,radiance,saturated,sun_elevation,sun_azimuth,glint_angle,glint_score";

fn main() {
    let cli = Cli::parse();
    read::configure();
    let pool = |n: usize| rayon::ThreadPoolBuilder::new().num_threads(n.max(1)).build().unwrap();
    match &cli.cmd {
        Cmd::Detect { c, out } => {
            if c.bbox.is_none() && c.aoi.is_none() && c.region.is_none() { die("detect: provide --bbox, --aoi, or --region"); }
            run_detect(c, out, &pool(c.concurrency));
        }
        Cmd::Cluster { c, archive, out, min_dates, min_avg_b12, score_threshold, clouds, coverage_scan } => {
            if archive.is_none() && c.bbox.is_none() && c.aoi.is_none() && c.region.is_none() {
                die("cluster: provide --archive GLOB, or --bbox/--aoi/--region to detect fresh");
            }
            let opts = ClusterOptions { merge_distance: 135.0, min_dates: *min_dates,
                min_avg_b12: *min_avg_b12, observations: None, score_threshold: *score_threshold };
            run_cluster(c, archive, out, opts, clouds, coverage_scan, &pool(c.concurrency));
        }
    }
}

// grow the detection archive: one csv per scene, presence == done → resumable.
fn run_detect(c: &Common, out: &str, pool: &rayon::ThreadPool) {
    let t = c.thresholds();
    let aois = load_aois(c);
    let (start, end) = c.dates();
    let harmonize = c.harmonize();
    let reader = read::make_reader(c.gpu, c.region.is_some(), harmonize).unwrap_or_else(|e| die(&e));
    eprintln!("detect: {} aoi(s) | {start} → {end} | b12≥{} b11≥{} | source={}{} → {out}/",
        aois.len(), t.b12_min, t.b11_min, c.source, if c.gpu { " gpu" } else if c.region.is_some() { " bulk" } else { "" });
    let (mut scenes, mut detected, mut errored, mut search_errors) = (0usize, 0usize, 0usize, 0usize);
    for aoi in &aois {
        let mut items = match stac::search(aoi.bbox, &start, &end, c.cloud, &c.source) {
            Ok(v) => v,
            Err(e) => {
                search_errors += 1;
                eprintln!("  {} search FAIL: {e}", aoi.id);
                continue;
            }
        };
        filter_tiles(c, &mut items);
        scenes += items.len();
        // each scene reaches exactly one durable terminal state: a <mgrs>_<date>.csv
        // (success, header-only when flareless) or a sibling .err (a read/detect FAIL,
        // holding the message). a lone .err is NOT done → retried on the next resume,
        // and cleared the moment that scene succeeds. so a clean run leaves zero .err
        // and `verify` can PROVE no scene errored — see cloud/box.sh verify().
        let (done, skipped, errs, det) = pool.install(|| {
            items.par_iter().map(|item| {
                let path = format!("{out}/{}/{}_{}.csv", aoi.id, item.mgrs, item.date);
                let errp = format!("{out}/{}/{}_{}.err", aoi.id, item.mgrs, item.date);
                if Path::new(&path).exists() { return (0usize, 1usize, 0usize, 0usize); }
                match read::detect_scene(&*reader, item, det_bbox(aoi, item), aoi.full_tile, &t, false) {
                    Ok((dets, _)) => {
                        let _ = fs::create_dir_all(Path::new(&path).parent().unwrap());
                        let body: String = std::iter::once(SCENE_HEADER.to_string())
                            .chain(dets.iter().map(scene_row)).collect::<Vec<_>>().join("\n") + "\n";
                        let _ = fs::write(&path, body);
                        // the per-scene cloud-mask slice (.cld) rides the detections .csv:
                        // the persistence denominator folded into the detection pass, AOI-
                        // agnostic cells, sampled even on flareless scenes. presence of the
                        // .csv drives resumability — the .cld is written alongside it.
                        let cld = read::cloud_scene(item, det_bbox(aoi, item), aoi.full_tile);
                        let cbody: String = std::iter::once("glon,glat,date,cloud_frac".to_string())
                            .chain(cld.iter().map(|(k, f)| format!("{k},{},{f}", item.date))).collect::<Vec<_>>().join("\n") + "\n";
                        let _ = fs::write(format!("{out}/{}/{}_{}.cld", aoi.id, item.mgrs, item.date), cbody);
                        let _ = fs::remove_file(&errp);
                        eprintln!("  {} {}_{}: {} det", aoi.id, item.mgrs, item.date, dets.len());
                        (1, 0, 0, dets.len())
                    }
                    Err(e) => {
                        let _ = fs::create_dir_all(Path::new(&errp).parent().unwrap());
                        let _ = fs::write(&errp, format!("{e}\n"));
                        eprintln!("  {} {}_{} FAIL: {e}", aoi.id, item.mgrs, item.date);
                        (0, 0, 1, 0)
                    }
                }
            }).reduce(|| (0, 0, 0, 0), |x, y| (x.0 + y.0, x.1 + y.1, x.2 + y.2, x.3 + y.3))
        });
        detected += det; errored += errs;
        eprintln!("  {} {}: {} scenes ({} new, {} cached, {} errored), {} detections", aoi.id, aoi.name, items.len(), done, skipped, errs, det);
    }
    if search_errors > 0 {
        // Do not emit the `done:` sentinel: box.sh/watch and operators must not mistake
        // a catalogue with omitted AOIs for a complete run. Scene CSVs remain cached,
        // so rerunning is still presence==done/resumable.
        eprintln!("\nincomplete: {search_errors} AOI search(es) failed after retries; rerun to resume → {out}/");
        std::process::exit(1);
    }
    eprintln!("\ndone: {scenes} scenes, {errored} errored, {detected} detections → {out}/");
    eprintln!("archive to parquet, then: s2-flares cluster --archive '{out}/**/*.parquet' --out clusters.parquet");
}

// the derived cluster view — over the archive (--archive) or a fresh detect.
fn run_cluster(c: &Common, archive: &Option<String>, out: &Option<String>,
    mut opts: ClusterOptions, clouds: &Option<String>, coverage_scan: &Option<String>, pool: &rayon::ThreadPool) {
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
            let reader = read::make_reader(c.gpu, c.region.is_some(), harmonize).unwrap_or_else(|e| die(&e));
            eprintln!("cluster: fresh detect over {} aoi(s) | {start} → {end}", aois.len());
            let mut dets = Vec::new();
            let mut obs: std::collections::HashMap<String, bool> = std::collections::HashMap::new();
            for aoi in &aois {
                let mut items = match stac::search(aoi.bbox, &start, &end, c.cloud, &c.source) {
                    Ok(v) => v, Err(e) => { eprintln!("  {} search FAIL: {e}", aoi.id); continue; }
                };
                filter_tiles(c, &mut items);
                let res: Vec<(String, bool, Vec<Detection>)> = pool.install(|| items.par_iter().map(|item|
                    match read::detect_scene(&*reader, item, det_bbox(aoi, item), aoi.full_tile, &t, true) {
                        Ok((d, cf)) => (item.date.clone(), cf, d),
                        Err(e) => { eprintln!("  {} {}_{} FAIL: {e}", aoi.id, item.mgrs, item.date); (item.date.clone(), false, Vec::new()) }
                    }).collect());
                for (date, cf, d) in res { obs.insert(date, cf); dets.extend(d); }
                eprintln!("  {} {}: {} scenes", aoi.id, aoi.name, items.len());
            }
            (dets, Some(obs.values().filter(|v| **v).count()))
        }
    };

    opts.observations = observations;
    let mut clusters = cluster_detections(&detections, &opts);
    eprintln!("{} detections → {} clusters", detections.len(), clusters.len());

    // clear-sky persistence: the fold-in path joins each anchor's ~100 m cell against
    // the cloud mask emitted during detection (one SCL read, no second pass); the
    // legacy --coverage-scan re-reads SCL at every anchor (kept to cross-check). either
    // rescores with the measured n_clear_obs denominator via the same score_cluster.
    if let Some(glob) = clouds {
        clouds_rescore(glob, &start, &end, &mut clusters);
    } else if let (Some(dir), Some(arch)) = (coverage_scan, archive) {
        coverage_rescore(c, arch, dir, &start, &end, &mut clusters, pool);
    }

    match out {
        Some(path) => match view::write_view(&clusters, path) {
            Ok(()) => eprintln!("view → {path}"),
            Err(e) => die(&format!("write view: {e}")),
        },
        None => print!("{}", view::geojson(&clusters)),
    }
}

// the fold-in rescore: spatial-join each cluster anchor against the cloud mask
// (clouds/, emitted at detection). n_clear_obs = distinct dates where the anchor's
// ~100 m cell was clear (cloud_frac ≤ CLEAR_MAX), ∪ the site's own detection dates
// (a lit look is an observation; guarantees n_dates ⊆). a hash join on the snapped
// cell key — same grid both sides; widen to the 3×3 neighbourhood when the exact cell
// has no rows (a cell-edge anchor). a cell with no mask rows → observations left None
// (persistence skipped, as when coverage is absent). no STAC search, no second SCL pass.
fn clouds_rescore(glob: &str, start: &str, end: &str, clusters: &mut [Cluster]) {
    use std::collections::{HashMap, HashSet};
    let step = s2_flares_core::GRID_STEP;
    // the join only ever reads each anchor's own cell + its 3×3 fallback, so precompute
    // that cell-key set (≤ 9·clusters) and keep ONLY those while streaming the mask — peak
    // memory is O(anchors), not O(mask). materialising the whole multi-GB mask OOM'd the box.
    let mut needed: HashSet<String> = HashSet::new();
    for cl in clusters.iter() {
        for dj in -1..=1 { for di in -1..=1 {
            needed.insert(s2_flares_core::cell_key(cl.lon + di as f64 * step, cl.lat + dj as f64 * step));
        }}
    }
    // cell key → the distinct dates that cell was observed CLEAR (relevant cells only).
    let mut clear: HashMap<String, HashSet<String>> = HashMap::new();
    view::read_clouds(glob, start, end, |glon, glat, date, cf| {
        if cf <= s2_flares_core::CLEAR_MAX {
            let k = s2_flares_core::cell_key(glon, glat);
            if needed.contains(&k) { clear.entry(k).or_default().insert(date.to_string()); }
        }
    }).unwrap_or_else(|e| die(&e));
    let (mut rescored, mut joined) = (0usize, 0usize);
    for cl in clusters.iter_mut() {
        let mut dates: HashSet<String> = cl.detections.iter().map(|d| d.date.clone()).collect();
        // the anchor's own cell first; only if it carries no mask rows fall back to the
        // 3×3 neighbourhood (cell-edge anchor) — avoids inflating the denominator.
        let own = s2_flares_core::cell_key(cl.lon, cl.lat);
        let hit = if clear.contains_key(&own) {
            clear.get(&own).map(|s| { dates.extend(s.iter().cloned()); true }).unwrap_or(false)
        } else {
            let mut any = false;
            for dj in -1..=1 { for di in -1..=1 {
                if let Some(s) = clear.get(&s2_flares_core::cell_key(cl.lon + di as f64 * step, cl.lat + dj as f64 * step)) {
                    dates.extend(s.iter().cloned()); any = true;
                }
            }}
            any
        };
        if hit { joined += 1; cl.set_observations(dates.len()); rescored += 1; }
    }
    eprintln!("clouds: joined {joined} / {} clusters against the cloud mask ({rescored} rescored)", clusters.len());
}

// the coverage scan + rescore. resumable per-scene SCL sampling at the cluster
// anchors (presence == done), then n_clear_obs per site → Cluster::set_observations.
// clear == cloud fraction over the site window ≤ CLEAR_MAX (permian's clear-sky rule).
fn coverage_rescore(c: &Common, glob: &str, dir: &str, start: &str, end: &str,
    clusters: &mut [Cluster], pool: &rayon::ThreadPool) {
    const CLEAR_MAX: f64 = 0.10;
    let sites: Vec<Site> = clusters.iter().map(|c| Site { h3: c.id.clone(), lon: c.lon, lat: c.lat }).collect();
    if sites.is_empty() { return; }
    // per-tile STAC search → the unique acquisitions that can see any anchor (the
    // clear-but-unlit looks the detection archive can't supply — its own denominator).
    let tiles = view::tile_bboxes(glob, start, end).unwrap_or_else(|e| die(&e));
    let mut scenes: std::collections::HashMap<String, stac::Item> = std::collections::HashMap::new();
    for (mgrs, bb) in &tiles {
        match stac::search(*bb, start, end, 100.0, &c.source) {
            Ok(items) => for it in items { scenes.entry(it.id.clone()).or_insert(it); }
            Err(e) => eprintln!("  coverage search {mgrs} FAIL: {e}"),
        }
    }
    let items: Vec<stac::Item> = scenes.into_values().collect();
    let _ = fs::create_dir_all(dir);
    eprintln!("coverage: {} sites · {} scenes → {dir}/", sites.len(), items.len());
    // resumable per-scene scan: <mgrs>_<date>.csv = id,cloud_frac per in-footprint site.
    pool.install(|| items.par_iter().for_each(|it| {
        let path = format!("{dir}/{}_{}.csv", it.mgrs, it.date);
        if Path::new(&path).exists() { return; }
        let rows = read::cover_scene(it, &sites);
        let body: String = std::iter::once("id,cloud_frac".to_string())
            .chain(rows.iter().map(|(id, cf)| format!("{id},{cf}"))).collect::<Vec<_>>().join("\n") + "\n";
        let _ = fs::write(&path, body);
    }));
    // aggregate clear DATES per site id across the per-scene csvs (date is in the name).
    let mut clear: std::collections::HashMap<String, std::collections::HashSet<String>> = std::collections::HashMap::new();
    if let Ok(rd) = fs::read_dir(dir) {
        for ent in rd.flatten() {
            let name = ent.file_name().to_string_lossy().to_string();
            let date = match name.strip_suffix(".csv").and_then(|s| s.rsplit_once('_')) { Some((_, d)) => d.to_string(), None => continue };
            let text = match fs::read_to_string(ent.path()) { Ok(t) => t, Err(_) => continue };
            for line in text.lines().skip(1) {
                if let Some((id, cf)) = line.split_once(',') {
                    if cf.parse::<f64>().map(|v| v <= CLEAR_MAX).unwrap_or(false) {
                        clear.entry(id.to_string()).or_default().insert(date.clone());
                    }
                }
            }
        }
    }
    // n_clear_obs = |clear looks ∪ the site's own detection dates| (guarantees n_dates ⊆).
    let mut rescored = 0usize;
    for cl in clusters.iter_mut() {
        let mut dates: std::collections::HashSet<String> = cl.detections.iter().map(|d| d.date.clone()).collect();
        if let Some(cd) = clear.get(&cl.id) { dates.extend(cd.iter().cloned()); rescored += 1; }
        cl.set_observations(dates.len());
    }
    eprintln!("coverage: rescored {rescored} / {} clusters with clear-sky persistence", clusters.len());
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
