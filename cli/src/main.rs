//! Native Sentinel-2 emissions CLI. `detect` writes independent canonical GeoJSON
//! flare, plume and cloud analyses; `archive` publishes them unchanged and
//! `cluster` derives persistent flare sites from them. `--shard`,
//! `verify` and `coverage` are what a fleet needs, so the orchestrator can stay
//! generic and hold no knowledge of the record layout.

mod archive;
mod detect;
mod discover;
#[cfg(feature = "gpu")]
mod gpu;
mod manifest;
mod models;
mod plume;
mod read;
mod record;
mod review;
mod stac;
mod verify;
mod view;

use clap::{Args as ClapArgs, Parser, Subcommand, ValueEnum};
use s2e_core::{cluster_detections, pad_bbox, Cluster, ClusterOptions, Thresholds};
use std::fs;
use std::path::Path;

/// Native Sentinel-2 flare and methane-plume detection.
#[derive(Parser)]
#[command(name = "s2e", version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

// `detect` carries the whole shared option block and the others no longer do,
// so the variants are lopsided. one of these is parsed per process — boxing it
// would buy an allocation and nothing else.
#[allow(clippy::large_enum_variant)]
#[derive(Subcommand)]
enum Cmd {
    /// Detect emissions into independent, resumable GeoJSON analysis records.
    Detect {
        /// Output directory for canonical observations/ and optional assets/.
        #[arg(long, value_name = "DIR", default_value = "out")]
        out: String,
        /// Scene manifest from `s2e discover`, read instead of searching the
        /// catalogue. A fleet member is given one so it never searches: discovery
        /// happens once for the whole campaign and every member works the identical
        /// scene list. Without it, each feature is searched as it comes.
        #[arg(long, value_name = "FILE")]
        scenes: Option<String>,
        /// Detector mode. AOI runs default to both related S2 signals; whole-tile
        /// region scans currently support the flare mode only.
        #[arg(long, value_enum, default_value_t = DetectorMode::Both)]
        mode: DetectorMode,
        /// Model/cache directory (default: $S2_MODELS or ~/.cache/s2e/models).
        #[arg(long, value_name = "DIR")]
        models: Option<String>,
        /// Fixed wind components for reproducible/offline plume runs. Supply both;
        /// otherwise NASA GEOS-FP is fetched for each acquisition hour.
        #[arg(long, requires = "wind_v", allow_hyphen_values = true)]
        wind_u: Option<f32>,
        #[arg(long, requires = "wind_u", allow_hyphen_values = true)]
        wind_v: Option<f32>,
        // Common (with its knobs help-heading) goes last so the heading doesn't leak.
        #[command(flatten)]
        c: Common,
    },
    /// Resolve the whole AOI's scene list once, into a manifest a fleet shares.
    ///
    /// Discovery is the fragile half of a bulk run: one catalogue search per feature,
    /// all of it before any reading, against an endpoint that sheds bursts. This
    /// batches the AOI into as few searches as the endpoint will take and writes what
    /// they return, so a member never searches, a resume costs nothing, and every
    /// member works from the same scenes.
    Discover {
        /// Manifest to write (gzipped NDJSON: a header line, then a scene per line).
        #[arg(long, value_name = "FILE", default_value = "scenes.ndjson.gz")]
        out: String,
        /// AOI features per catalogue request. They are sent as one MultiPolygon, so
        /// this is the batching factor; the endpoint rejects a request carrying the
        /// whole of a large AOI, and a hundred is comfortably inside that.
        #[arg(long, value_name = "N", default_value_t = 100)]
        batch: usize,
        /// Days either side of the window to cover, so a plume run's background
        /// selection reads from the manifest too rather than going to the network.
        #[arg(long, value_name = "DAYS", default_value_t = detect::PLUME_PAD_DAYS)]
        pad: i64,
        /// Split the window into this many sub-windows and resolve them at once. The
        /// requests still share one pace, so this overlaps the waiting rather than
        /// asking faster. Each job holds its own scene list, so raise it on a machine
        /// with the memory for it, not on the control plane.
        #[arg(long, value_name = "N", default_value_t = 1)]
        jobs: usize,
        #[command(flatten)]
        c: Common,
    },
    /// Cluster detections into persistent flare sites. Publishing is the ETL
    /// repository's job: this writes the sites and their membership, and nothing
    /// that counts days — only the table holding the observations can do that.
    Cluster {
        /// Detections parquet: id, lon, lat, date, max_b12, max_b11,
        /// b12_b11_ratio, radiance, sun_elevation, glint_score.
        #[arg(long, value_name = "FILE")]
        detections: String,
        /// One row per cluster: id, position, score, flags and the cluster-level
        /// measures the clustering itself produced.
        #[arg(long, value_name = "FILE")]
        clusters: String,
        /// One row per detection that joined a cluster: id, site_id.
        #[arg(long, value_name = "FILE")]
        members: String,
        /// Cloud-mask parquet (lon, lat, date, cloud) for the persistence
        /// denominator: spatial-join each cluster anchor's ~100 m cell →
        /// n_clear_obs = distinct clear dates, which rescores the site.
        #[arg(long, value_name = "FILE")]
        clouds: Option<String>,
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
        /// Window start (default ~6 months ago).
        #[arg(long, value_name = "Y-M-D")]
        start: Option<String>,
        /// Window end (default today).
        #[arg(long, value_name = "Y-M-D")]
        end: Option<String>,
    },
    /// Fetch and verify the pinned upstream MARS-S2L + CloudSEN checkpoints.
    Models {
        #[arg(long, value_name = "DIR")]
        dir: Option<String>,
    },
    /// Publish canonical GeoJSON records and assets unchanged.
    Archive {
        /// Detection output containing observations/ and optional assets/.
        #[arg(long, value_name = "DIR", default_value = "out")]
        input: String,
        /// Local directory or s3:// bucket/prefix.
        #[arg(long, value_name = "PATH")]
        destination: Option<String>,
    },
    /// Deterministic plume triage: score the published plume detections (wind
    /// consistency, fixed-offset recurrence, scene-day regimes, magnitude prior,
    /// scene hygiene, optional OSM collinearity) into a ranked candidate list for
    /// valid-plume curation (etl/providers/data-desk/burnoff/sql/valid-plumes.txt).
    /// Never mutates canonical records; rebuild the tables first.
    Review {
        /// Store ROOT containing data-desk/detections (local dir or s3:// prefix).
        #[arg(long, value_name = "ROOT")]
        root: String,
        /// Linear features GeoJSON (OSM roads/boundaries/waterways/hedges) for
        /// the collinearity check; reads probability assets under ROOT.
        #[arg(long, value_name = "FILE")]
        lines: Option<String>,
        /// Output CSV (default stdout).
        #[arg(long, value_name = "FILE")]
        out: Option<String>,
    },
    /// Prove a detect output is complete before it is archived: every requested
    /// AOI feature has a durable record and no retryable `.err` remains. Exits
    /// non-zero on a gap, which is a fleet's signal to re-run rather than tear down.
    Verify {
        /// Detection output containing observations/.
        #[arg(long, value_name = "DIR", default_value = "out")]
        input: String,
        /// The AOI the run was given; omit for a bbox/region run.
        #[arg(long, value_name = "FILE")]
        aoi: Option<String>,
        /// Check only this member's slice, exactly as passed to `detect`.
        #[arg(long, value_name = "I/N", value_parser = parse_shard)]
        shard: Option<(usize, usize)>,
    },
    /// Merge a scanned AOI into the published coverage.geojson under ROOT.
    Coverage {
        /// Store ROOT that receives coverage.geojson (local dir or s3:// prefix).
        #[arg(long, value_name = "ROOT")]
        root: String,
        /// The AOI that was scanned.
        #[arg(long, value_name = "FILE")]
        aoi: String,
        /// The window it was scanned over, stamped onto each entry.
        #[arg(long, value_name = "Y-M-D", default_value = "2015-01-01")]
        start: String,
        #[arg(long, value_name = "Y-M-D", default_value = "2100-01-01")]
        end: String,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum DetectorMode {
    Both,
    Flares,
    Plumes,
}

/// options shared by both subcommands: the area, the search window, the reader
/// profile, and the flare detector's spectral knobs.
#[derive(ClapArgs)]
struct Common {
    /// Area of interest as West,South,East,North.
    #[arg(long, value_name = "W,S,E,N", value_parser = parse_bbox, allow_hyphen_values = true)]
    bbox: Option<[f64; 4]>,
    /// AOI geojson FeatureCollection (one run per feature).
    #[arg(long, value_name = "FILE")]
    aoi: Option<String>,
    /// Take only member I of an N-way round-robin split of --aoi, so a fleet can
    /// share one file and each member work a balanced slice of it.
    #[arg(long, value_name = "I/N", value_parser = parse_shard)]
    shard: Option<(usize, usize)>,
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
    /// Imagery profile. L1C is canonical on CloudFerro; L2A profiles remain for
    /// archive comparison and browser-compatible COG reads.
    #[arg(long, default_value = "aws-l1c", value_parser = ["aws", "aws-l1c", "cdse", "cdse-l1c"])]
    source: String,
    /// Scenes in flight.
    #[arg(long, default_value_t = 4)]
    concurrency: usize,
    #[command(flatten)]
    knobs: Knobs,
}

/// flare detector floors. The compact-source morphology gates remain at their
/// validated defaults; these flags expose the useful radiometric adjustments.
#[derive(ClapArgs)]
#[command(next_help_heading = "Flare detector knobs")]
struct Knobs {
    /// B12 swir-hot reflectance floor.
    #[arg(long, default_value_t = 0.30)]
    b12_min: f64,
    /// B11 swir-hot reflectance floor.
    #[arg(long, default_value_t = 0.20)]
    b11_min: f64,
    /// Brightest-pixel B12 floor.
    #[arg(long, default_value_t = 0.50)]
    peak_b12_min: f64,
    /// Flare-vs-background contrast ratio.
    #[arg(long, default_value_t = 3.0)]
    contrast_ratio: f64,
    /// Background reflectance floor.
    #[arg(long, default_value_t = 0.15)]
    background_floor: f64,
    /// Spatial peakedness gate.
    #[arg(long, default_value_t = 1.15)]
    peakedness_min: f64,
    /// Hot-core B12 floor: the `pixels`/`radiance` flare-size measurement counts
    /// only pixels above this (combustion-hot), not the loose detection mask.
    #[arg(long, default_value_t = 0.50)]
    hot_floor: f64,
}

/// the search window, defaulted: ~6 months back to today.
fn window(start: &Option<String>, end: &Option<String>) -> (String, String) {
    (
        start.clone().unwrap_or_else(|| days_ago(183)),
        end.clone().unwrap_or_else(today),
    )
}

impl Common {
    fn dates(&self) -> (String, String) {
        window(&self.start, &self.end)
    }
    fn thresholds(&self) -> Thresholds {
        let k = &self.knobs;
        Thresholds {
            b12_min: k.b12_min,
            b11_min: k.b11_min,
            peak_b12_min: k.peak_b12_min,
            contrast_ratio: k.contrast_ratio,
            background_floor: k.background_floor,
            peakedness_min: k.peakedness_min,
            hot_floor: k.hot_floor,
            ..Default::default()
        }
    }
}

fn parse_shard(s: &str) -> Result<(usize, usize), String> {
    let (i, n) = s.split_once('/').ok_or("expected I/N")?;
    let parse = |x: &str| x.trim().parse::<usize>().map_err(|e| e.to_string());
    let (i, n) = (parse(i)?, parse(n)?);
    if i < n {
        Ok((i, n))
    } else {
        Err("expected 0 <= I < N".into())
    }
}

fn parse_bbox(s: &str) -> Result<[f64; 4], String> {
    let v: Vec<f64> = s
        .split(',')
        .map(|x| x.trim().parse())
        .collect::<Result<_, _>>()
        .map_err(|e| format!("not a number: {e}"))?;
    v.try_into().map_err(|_| "expected W,S,E,N".into())
}

struct Aoi {
    id: String,
    name: String,
    bbox: [f64; 4],
    full_tile: bool,
    geometry: serde_json::Value,
    properties: serde_json::Map<String, serde_json::Value>,
    key: String,
}

fn die(msg: &str) -> ! {
    eprintln!("{msg}");
    std::process::exit(1);
}

// --- aoi loading -------------------------------------------------------------
fn geom_bbox(geom: &serde_json::Value) -> [f64; 4] {
    let (mut w, mut s, mut e, mut n) = (
        f64::INFINITY,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NEG_INFINITY,
    );
    fn walk(c: &serde_json::Value, w: &mut f64, s: &mut f64, e: &mut f64, n: &mut f64) {
        if let Some(arr) = c.as_array() {
            if arr.first().and_then(|x| x.as_f64()).is_some() && arr.len() >= 2 {
                let (x, y) = (arr[0].as_f64().unwrap(), arr[1].as_f64().unwrap());
                *w = w.min(x);
                *e = e.max(x);
                *s = s.min(y);
                *n = n.max(y);
            } else {
                for x in arr {
                    walk(x, w, s, e, n);
                }
            }
        }
    }
    walk(&geom["coordinates"], &mut w, &mut s, &mut e, &mut n);
    [w, s, e, n]
}

/// The features of an AOI file, each with its position in the WHOLE collection, so a
/// shard's ids do not shift with fleet size. `--shard I/N` keeps every Nth from I.
fn aoi_features(path: &str, shard: Option<(usize, usize)>) -> Vec<(usize, serde_json::Value)> {
    let text = fs::read_to_string(path).unwrap_or_else(|e| die(&format!("read aoi: {e}")));
    let gj: serde_json::Value =
        serde_json::from_str(&text).unwrap_or_else(|e| die(&format!("parse aoi: {e}")));
    let all = gj["features"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .enumerate();
    match shard {
        Some((i, n)) => all.skip(i).step_by(n).collect(),
        None => all.collect(),
    }
}

/// The record identity of an AOI feature: our own `id`, GEM's `ProjectID`, else
/// its position. Detect, verify and coverage must all agree on this.
fn feature_id(idx: usize, f: &serde_json::Value) -> String {
    let p = &f["properties"];
    p["id"]
        .as_str()
        .or_else(|| p["ProjectID"].as_str())
        .map(String::from)
        .unwrap_or_else(|| idx.to_string())
}

fn load_aois(c: &Common) -> Vec<Aoi> {
    // --region: one wide-area job, scenes detected over their whole tile (full_tile).
    if let Some(b) = c.region {
        let geometry = record::bbox_geometry(b);
        let key = record::area_key("region", &serde_json::json!({"geometry":geometry,"bbox":b}));
        return vec![Aoi {
            id: "region".into(),
            name: String::new(),
            bbox: b,
            full_tile: true,
            key,
            geometry,
            properties: Default::default(),
        }];
    }
    if let Some(b) = c.bbox {
        let geometry = record::bbox_geometry(b);
        let key = record::area_key("aoi", &serde_json::json!({"geometry":geometry,"bbox":b}));
        return vec![Aoi {
            id: "aoi".into(),
            name: String::new(),
            bbox: b,
            full_tile: false,
            key,
            geometry,
            properties: Default::default(),
        }];
    }
    aoi_features(c.aoi.as_ref().unwrap(), c.shard)
        .into_iter()
        .map(|(idx, f)| {
            let p = &f["properties"];
            let id = feature_id(idx, &f);
            let name = p["name"]
                .as_str()
                .or_else(|| p["TerminalName"].as_str())
                .unwrap_or("")
                .to_string();
            let geometry = f["geometry"].clone();
            let bbox = pad_bbox(geom_bbox(&geometry), c.buffer);
            Aoi {
                key: record::area_key(&id, &serde_json::json!({"geometry":geometry,"bbox":bbox})),
                id,
                name,
                bbox,
                full_tile: false,
                geometry,
                properties: p.as_object().cloned().unwrap_or_default(),
            }
        })
        .collect()
}

/// Read the manifest a run was given and prove it fits: same catalogue, same AOI, and
/// a window covering what this mode needs — plume selection reaches `PLUME_PAD_DAYS`
/// either side of the requested dates. A manifest that does not fit stops the run
/// rather than being ignored, because ignoring it would quietly search the network for
/// thousands of features and look like nothing was wrong.
fn load_manifest(c: &Common, path: &str, mode: DetectorMode) -> Vec<stac::Item> {
    let Some(aoi) = c.aoi.as_deref() else {
        die("detect --scenes: a manifest is built for an --aoi, so the run needs the same one");
    };
    let (start, end) = c.dates();
    let (need_start, need_end) = match mode {
        DetectorMode::Flares => (start, end),
        _ => (
            shift_date(&start, -detect::PLUME_PAD_DAYS),
            shift_date(&end, detect::PLUME_PAD_DAYS),
        ),
    };
    let sha = manifest::aoi_sha256(aoi).unwrap_or_else(|e| die(&e));
    let (header, items) =
        manifest::read(Path::new(path)).unwrap_or_else(|e| die(&format!("detect --scenes: {e}")));
    manifest::check(&header, &c.source, &need_start, &need_end, &sha)
        .unwrap_or_else(|e| die(&format!("detect --scenes: {e}")));
    eprintln!(
        "scenes: {} from {path} · {} -> {} · built {} in {} requests",
        items.len(),
        header.start,
        header.end,
        header.created,
        header.requests
    );
    items
}

// the per-scene detection region: a whole tile (full_tile/--region wide-area) or the
// query window. orthogonal to reader choice — the driver just passes this as `region`.
fn det_bbox(aoi: &Aoi, item: &stac::Item) -> [f64; 4] {
    if aoi.full_tile {
        item.bbox
    } else {
        aoi.bbox
    }
}

// restrict a scene list to --tiles when given (a filter over the region search).
fn filter_tiles(c: &Common, items: &mut Vec<stac::Item>) {
    if !c.tiles.is_empty() {
        items.retain(|i| c.tiles.contains(&i.mgrs));
    }
}

fn main() {
    let cli = Cli::parse();
    read::configure();
    let pool = |n: usize| {
        rayon::ThreadPoolBuilder::new()
            .num_threads(n.max(1))
            .build()
            .unwrap()
    };
    match &cli.cmd {
        Cmd::Detect {
            c,
            out,
            scenes,
            mode,
            models,
            wind_u,
            wind_v,
        } => {
            if c.bbox.is_none() && c.aoi.is_none() && c.region.is_none() {
                die("detect: provide --bbox, --aoi, or --region");
            }
            let manifest = scenes.as_deref().map(|path| load_manifest(c, path, *mode));
            let scenes = manifest.as_deref();
            if c.region.is_some() {
                if *mode != DetectorMode::Flares {
                    die("detect --mode both/plumes needs point or AOI targets; use --mode flares for a whole-tile --region scan");
                }
                detect::run_flares(c, out, scenes, &pool(c.concurrency));
            } else if c.source.ends_with("l1c") {
                let fixed = wind_u.zip(*wind_v).map(|(u, v)| [u, v]);
                detect::run_targeted(c, out, scenes, *mode, models.as_deref(), fixed);
            } else if *mode == DetectorMode::Flares {
                detect::run_flares(c, out, scenes, &pool(c.concurrency));
            } else {
                die("methane plume detection requires an L1C --source");
            }
        }
        Cmd::Discover {
            c,
            out,
            batch,
            pad,
            jobs,
        } => discover::run(c, out, *batch, *pad, *jobs),
        Cmd::Cluster {
            detections,
            clusters: out,
            members,
            clouds,
            min_dates,
            min_avg_b12,
            score_threshold,
            start,
            end,
        } => {
            let (start, end) = window(start, end);
            eprintln!("cluster: {detections} | {start} → {end}");
            let dets = view::read_detections(detections, &start, &end).unwrap_or_else(|e| die(&e));
            let mut clusters = cluster_detections(
                &dets,
                &ClusterOptions {
                    merge_distance: 135.0,
                    min_dates: *min_dates,
                    min_avg_b12: *min_avg_b12,
                    observations: None,
                    score_threshold: *score_threshold,
                },
            );
            eprintln!("{} detections → {} clusters", dets.len(), clusters.len());
            // clear-sky persistence: join each anchor's ~100 m cell against the
            // cloud mask emitted during detection (one SCL read, no second pass)
            // and rescore on the measured n_clear_obs denominator.
            if let Some(path) = clouds {
                clouds_rescore(path, &start, &end, &mut clusters);
            }
            view::write(&clusters, out, members).unwrap_or_else(|e| die(&e));
            eprintln!("clusters → {out} · members → {members}");
        }
        Cmd::Models { dir } => {
            let dir = dir
                .as_ref()
                .map(Path::new)
                .map(Path::to_path_buf)
                .unwrap_or_else(models::ModelPaths::default_dir);
            let paths = models::ModelPaths::ensure(&dir).unwrap_or_else(|e| die(&e));
            // Loading proves that both original PyTorch state dicts are structurally valid.
            models::MarsModel::load(&paths.mars).unwrap_or_else(|e| die(&e));
            models::CloudModel::load(&paths.clouds).unwrap_or_else(|e| die(&e));
            println!("models ready: {}", dir.display());
        }
        Cmd::Archive { input, destination } => {
            let destination = destination.as_deref().unwrap_or(input);
            archive::publish(Path::new(input), destination)
                .unwrap_or_else(|e| die(&format!("archive: {e}")));
            println!("archive ready: {destination}");
        }
        Cmd::Review { root, lines, out } => {
            review::run(root, lines.as_deref(), out.as_deref())
                .unwrap_or_else(|e| die(&format!("review: {e}")));
        }
        Cmd::Verify { input, aoi, shard } => {
            let expected = aoi.as_deref().map(|path| {
                aoi_features(path, *shard)
                    .iter()
                    .map(|(idx, f)| feature_id(*idx, f))
                    .collect::<Vec<_>>()
            });
            if !verify::run(Path::new(input), expected.as_deref()) {
                std::process::exit(1);
            }
        }
        Cmd::Coverage {
            root,
            aoi,
            start,
            end,
        } => {
            let features: Vec<(String, serde_json::Value)> = aoi_features(aoi, None)
                .into_iter()
                .map(|(idx, f)| (feature_id(idx, &f), f))
                .collect();
            let (merged, total) = archive::coverage(root, &features, start, end, &today())
                .unwrap_or_else(|e| die(&format!("coverage: {e}")));
            println!("coverage: {merged} scanned features merged → {total} total");
        }
    }
}

fn shift_date(date: &str, days: i64) -> String {
    use chrono::{Duration, NaiveDate};
    (NaiveDate::parse_from_str(date, "%Y-%m-%d")
        .unwrap_or_else(|e| die(&format!("date {date}: {e}")))
        + Duration::days(days))
    .format("%Y-%m-%d")
    .to_string()
}

fn aoi_fits_chip(aoi: &Aoi, chip: &plume::Chip, epsg: i32) -> bool {
    if aoi.full_tile {
        return false;
    }
    let (zone, north) = s2e_core::utm_params(epsg);
    let corners = [
        (aoi.bbox[0], aoi.bbox[1]),
        (aoi.bbox[0], aoi.bbox[3]),
        (aoi.bbox[2], aoi.bbox[1]),
        (aoi.bbox[2], aoi.bbox[3]),
    ];
    corners.into_iter().all(|(lon, lat)| {
        let (x, y) = s2e_core::wgs84_to_utm(lon, lat, zone, north);
        x >= chip.min_x
            && x <= chip.min_x + chip.width as f64 * 10.0
            && y <= chip.max_y
            && y >= chip.max_y - chip.height as f64 * 10.0
    })
}

// the fold-in rescore: spatial-join each cluster anchor against the cloud mask
// emitted at detection. n_clear_obs = distinct dates where the anchor's ~100 m
// cell was clear (cloud ≤ CLEAR_MAX), ∪ the site's own detection dates (a lit
// look is an observation; guarantees n_dates ⊆). a hash join on the snapped cell
// key — same grid both sides; widen to the 3×3 neighbourhood when the exact cell
// has no rows (a cell-edge anchor). a cell with no mask rows is left unrescored,
// keeping the persistence term at 0 rather than inventing a denominator.
fn clouds_rescore(path: &str, start: &str, end: &str, clusters: &mut [Cluster]) {
    use std::collections::{HashMap, HashSet};
    let step = s2e_core::GRID_STEP;
    // the join only ever reads each anchor's own cell + its 3×3 fallback, so precompute
    // that cell-key set (≤ 9·clusters) and keep ONLY those while streaming the mask — peak
    // memory is O(anchors), not O(mask). materialising the whole multi-GB mask OOM'd the box.
    let mut needed: HashSet<String> = HashSet::new();
    let mut cells: HashSet<(i64, i64)> = HashSet::new();
    for cl in clusters.iter() {
        for dj in -1..=1 {
            for di in -1..=1 {
                let (lon, lat) = (cl.lon + di as f64 * step, cl.lat + dj as f64 * step);
                needed.insert(s2e_core::cell_key(lon, lat));
                cells.insert(((lon / step).round() as i64, (lat / step).round() as i64));
            }
        }
    }
    // cell key → the distinct dates that cell was observed CLEAR (relevant cells only).
    let mut clear: HashMap<String, HashSet<String>> = HashMap::new();
    view::read_clouds(path, start, end, &cells, |lon, lat, date, cf| {
        if cf <= s2e_core::CLEAR_MAX {
            let k = s2e_core::cell_key(lon, lat);
            if needed.contains(&k) {
                clear.entry(k).or_default().insert(date.to_string());
            }
        }
    })
    .unwrap_or_else(|e| die(&e));
    let mut rescored = 0usize;
    for cl in clusters.iter_mut() {
        let mut dates: HashSet<String> = cl.members.iter().map(|m| m.date.clone()).collect();
        // the anchor's own cell first; only if it carries no mask rows fall back to the
        // 3×3 neighbourhood (cell-edge anchor) — avoids inflating the denominator.
        let own = s2e_core::cell_key(cl.lon, cl.lat);
        let hit = if clear.contains_key(&own) {
            clear
                .get(&own)
                .map(|s| {
                    dates.extend(s.iter().cloned());
                    true
                })
                .unwrap_or(false)
        } else {
            let mut any = false;
            for dj in -1..=1 {
                for di in -1..=1 {
                    if let Some(s) = clear.get(&s2e_core::cell_key(
                        cl.lon + di as f64 * step,
                        cl.lat + dj as f64 * step,
                    )) {
                        dates.extend(s.iter().cloned());
                        any = true;
                    }
                }
            }
            any
        };
        if hit {
            cl.set_observations(dates.len());
            rescored += 1;
        }
    }
    eprintln!(
        "clouds: rescored {rescored} / {} clusters against the cloud mask",
        clusters.len()
    );
}

// --- minimal civil date helpers (avoid a chrono dependency) ------------------
fn epoch_days() -> i64 {
    (std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        / 86400) as i64
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
fn today() -> String {
    ymd(epoch_days())
}
fn days_ago(n: i64) -> String {
    ymd(epoch_days() - n)
}
