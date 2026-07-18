//! Detection application layer. Expensive L1C ingestion is shared here, while
//! each detector commits an independent GeoJSON analysis record.

use super::{models, plume, read, record, stac, Aoi, Common, DetectorMode};
use rayon::prelude::*;
use s2_flares_core::{Detection, Thresholds};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

fn method(name: &str, parameters: Value) -> Value {
    let mut value = json!({
        "name": name,
        "implementation": format!("s2-flares/{}", env!("CARGO_PKG_VERSION")),
        "parameters": parameters
    });
    value["fingerprint"] = record::fingerprint(&value).into();
    value
}

fn flare_method(thresholds: &Thresholds, radiometry: &str) -> Value {
    method(
        "sentinel-2-swir-flare",
        json!({"thresholds": thresholds, "radiometry": radiometry}),
    )
}

fn cloudsen_method() -> Value {
    method(
        "cloudsen12-v2",
        json!({"model_sha256": models::CLOUD_SHA256, "clear_classes": [0,1,3]}),
    )
}

fn scl_method() -> Value {
    method(
        "sentinel-2-scl",
        json!({"clear_max": s2_flares_core::CLEAR_MAX}),
    )
}

fn plume_method() -> Value {
    method(
        "mars-s2l-20250326",
        json!({
            "model_sha256": models::MARS_SHA256,
            "threshold": s2_flares_core::plume::DEFAULT_THRESHOLD,
            "min_pixels": s2_flares_core::plume::DEFAULT_MIN_PIXELS
        }),
    )
}

fn common_analysis(
    detector: &str,
    method: &Value,
    aoi: &Aoi,
    item: &stac::Item,
    source: &str,
    processed: Value,
    status: &str,
) -> Value {
    json!({
        "id": record::fingerprint(&json!({
            "detector": detector,
            "method": method["fingerprint"],
            "area": aoi.key,
            "scene": item.id
        })),
        "detector": detector,
        "status": status,
        "method": method,
        "scene": record::scene(item, source),
        "target": record::target(&aoi.id, &aoi.name, &aoi.geometry, &aoi.properties),
        "area": {
            "key": aoi.key,
            "requested": record::bbox_geometry(aoi.bbox),
            "processed": processed
        }
    })
}

fn path(root: &Path, aoi: &Aoi, item: &stac::Item, detector: &str, method: &Value) -> PathBuf {
    record::analysis_path(
        root,
        &aoi.key,
        &item.id,
        detector,
        method["fingerprint"].as_str().unwrap(),
    )
}

fn complete(root: &Path, aoi: &Aoi, item: &stac::Item, detector: &str, method: &Value) -> bool {
    let fingerprint = method["fingerprint"].as_str().unwrap();
    let complete = record::is_complete(
        &path(root, aoi, item, detector, method),
        detector,
        &item.id,
        fingerprint,
    );
    if complete {
        let _ = record::clear_error(&record::error_path(root, &aoi.key, &item.id, detector));
    }
    complete
}

fn flare_features(detections: &[Detection]) -> Vec<Value> {
    detections
        .iter()
        .map(|d| {
            let mut properties = serde_json::to_value(d).unwrap_or(Value::Null);
            if let Some(map) = properties.as_object_mut() {
                // positional/bookkeeping fields plus scene-level constants: the
                // latter live once on the analysis, not on every feature.
                for key in [
                    "lon",
                    "lat",
                    "peak_img_row",
                    "peak_img_col",
                    "date",
                    "mgrs",
                    "scene",
                    "sun_elevation",
                    "sun_azimuth",
                    "glint_angle",
                    "glint_score",
                ] {
                    map.remove(key);
                }
            }
            record::feature(
                json!({"type":"Point","coordinates":[d.lon,d.lat]}),
                properties,
            )
        })
        .collect()
}

/// scene-level glint terms on the flare analysis (formerly duplicated per feature).
fn flare_glint(analysis: &mut Value, item: &stac::Item) {
    if let Some(elevation) = item.sun_elevation {
        let angle = s2_flares_core::score::glint_angle_nadir(elevation);
        analysis["glint_angle"] = json!(angle);
        analysis["glint_score"] = json!(s2_flares_core::score::glint_score_from_angle(angle));
    }
}

fn cloud_features(cells: &[(String, f64)]) -> Vec<Value> {
    cells
        .iter()
        .filter_map(|(key, cloud_fraction)| {
            let mut xy = key.split(',').filter_map(|x| x.parse::<f64>().ok());
            let (lon, lat) = (xy.next()?, xy.next()?);
            Some(record::feature(
                json!({"type":"Point","coordinates":[lon,lat]}),
                json!({"cloud_fraction":cloud_fraction}),
            ))
        })
        .collect()
}

fn plume_features(chip: &plume::Chip, result: &plume::PlumeResult) -> Vec<Value> {
    result
        .plumes
        .iter()
        .enumerate()
        .map(|(index, detection)| {
            record::feature(
                plume::component_geometry(chip, detection),
                json!({
                    "id": format!("plume-{}", index + 1),
                    "pixels": detection.pixels,
                    "flux_rate_kg_h": detection.flux_rate,
                    "flux_rate_std_kg_h": detection.flux_rate_std,
                    "max_probability": detection.max_probability,
                    "centre": detection.centre
                }),
            )
        })
        .collect()
}

fn commit(
    root: &Path,
    aoi: &Aoi,
    item: &stac::Item,
    detector: &str,
    method: &Value,
    collection: Value,
) -> Result<(), String> {
    let error = record::error_path(root, &aoi.key, &item.id, detector);
    match record::atomic_write(&path(root, aoi, item, detector, method), &collection) {
        Ok(()) => record::clear_error(&error),
        Err(message) => {
            record::persist_error(&error, &message);
            Err(message)
        }
    }
}

struct CloudRecord<'a> {
    method: &'a Value,
    footprint: Value,
    status: &'a str,
    cells: &'a [(String, f64)],
}

fn write_clouds(
    root: &Path,
    c: &Common,
    aoi: &Aoi,
    item: &stac::Item,
    cloud: CloudRecord<'_>,
) -> Result<PathBuf, String> {
    let record_path = path(root, aoi, item, "clouds", cloud.method);
    if !complete(root, aoi, item, "clouds", cloud.method) {
        let analysis = common_analysis(
            "clouds",
            cloud.method,
            aoi,
            item,
            &c.source,
            cloud.footprint,
            cloud.status,
        );
        commit(
            root,
            aoi,
            item,
            "clouds",
            cloud.method,
            record::collection(analysis, cloud_features(cloud.cells)),
        )?;
    }
    Ok(record_path)
}

fn relative_record_name(path: &Path) -> String {
    path.file_name().unwrap().to_string_lossy().into_owned()
}

pub fn run_targeted(
    c: &Common,
    out: &str,
    mode: DetectorMode,
    model_dir: Option<&str>,
    fixed_wind: Option<[f32; 2]>,
) {
    let root = Path::new(out);
    let model_dir = model_dir
        .map(PathBuf::from)
        .unwrap_or_else(models::ModelPaths::default_dir);
    let paths = if mode == DetectorMode::Flares {
        models::ModelPaths::ensure_clouds(&model_dir)
    } else {
        models::ModelPaths::ensure(&model_dir)
    }
    .unwrap_or_else(|e| super::die(&e));
    let clouds = models::CloudModel::load(&paths.clouds).unwrap_or_else(|e| super::die(&e));
    let mars = if mode != DetectorMode::Flares {
        Some(models::MarsModel::load(&paths.mars).unwrap_or_else(|e| super::die(&e)))
    } else {
        None
    };
    let plume_detector = mars
        .as_ref()
        .map(|mars| plume::PlumeDetector::new(&clouds, mars, &model_dir, fixed_wind));
    let (cloud_method, flare_method, plume_method) = (
        cloudsen_method(),
        flare_method(&c.thresholds(), "l1c-toa"),
        plume_method(),
    );
    let aois = super::load_aois(c);
    let (start, end) = c.dates();
    let search_start = super::shift_date(&start, -120);
    let search_end = super::shift_date(&end, 120);
    let thresholds = c.thresholds();
    let flare_reader = read::make_reader(false, false).unwrap_or_else(|e| super::die(&e));
    let mut counts = [0usize; 5]; // scenes, cached, flares, plumes, errors
    eprintln!(
        "detect: mode={mode:?} · {} target(s) · L1C {start} -> {end} · canonical GeoJSON",
        aois.len()
    );
    for aoi in &aois {
        let lon = (aoi.bbox[0] + aoi.bbox[2]) * 0.5;
        let lat = (aoi.bbox[1] + aoi.bbox[3]) * 0.5;
        let mut candidates =
            match stac::search(aoi.bbox, &search_start, &search_end, 100.0, &c.source) {
                Ok(items) => items,
                Err(e) => {
                    eprintln!("  {} search FAIL: {e}", aoi.id);
                    counts[4] += 1;
                    continue;
                }
            };
        candidates.sort_by(|a, b| a.datetime.cmp(&b.datetime));
        let targets: Vec<_> = candidates
            .iter()
            .filter(|item| item.date >= start && item.date <= end)
            .filter(|item| item.cloud_cover.unwrap_or(100.0) <= c.cloud)
            .cloned()
            .collect();
        let mut cache = HashMap::new();
        eprintln!("  {} {}: {} acquisitions", aoi.id, aoi.name, targets.len());
        for item in &targets {
            counts[0] += 1;
            let want_flares = mode != DetectorMode::Plumes;
            let want_plumes = mode != DetectorMode::Flares;
            let flares_done = !want_flares || complete(root, aoi, item, "flares", &flare_method);
            let plumes_done = !want_plumes || complete(root, aoi, item, "plumes", &plume_method);
            if flares_done && plumes_done {
                counts[1] += 1;
                continue;
            }

            let chip = match plume::read_chip(item, lon, lat, &clouds) {
                Ok(chip) => chip,
                Err(e) => {
                    counts[4] += 1;
                    for detector in [
                        want_flares.then_some("flares"),
                        want_plumes.then_some("plumes"),
                    ]
                    .into_iter()
                    .flatten()
                    {
                        let error = record::error_path(root, &aoi.key, &item.id, detector);
                        record::persist_error(&error, &e);
                    }
                    eprintln!("    {} chip FAIL: {e}", item.id);
                    continue;
                }
            };
            cache.insert(item.id.clone(), chip.clone());
            let cloud_cells = plume::cloud_cells(&chip);
            let cloud_path = match write_clouds(
                root,
                c,
                aoi,
                item,
                CloudRecord {
                    method: &cloud_method,
                    footprint: chip.footprint(),
                    status: "ok",
                    cells: &cloud_cells,
                },
            ) {
                Ok(path) => path,
                Err(e) => {
                    counts[4] += 1;
                    eprintln!("    {} clouds FAIL: {e}", item.id);
                    continue;
                }
            };
            let cloud_ref = relative_record_name(&cloud_path);

            if want_flares && !flares_done {
                let detections = if super::aoi_fits_chip(aoi, &chip, item.epsg) {
                    Some(plume::flare_detections(&chip, item, &thresholds))
                } else {
                    match read::detect_scene(
                        &*flare_reader,
                        item,
                        super::det_bbox(aoi, item),
                        aoi.full_tile,
                        &thresholds,
                        false,
                    ) {
                        Ok((detections, _)) => Some(detections),
                        Err(e) => {
                            counts[4] += 1;
                            record::persist_error(
                                &record::error_path(root, &aoi.key, &item.id, "flares"),
                                &e,
                            );
                            eprintln!("    {} flares FAIL: {e}", item.id);
                            None
                        }
                    }
                };
                if let Some(mut detections) = detections {
                    detections.retain(|d| {
                        d.lon >= aoi.bbox[0]
                            && d.lon <= aoi.bbox[2]
                            && d.lat >= aoi.bbox[1]
                            && d.lat <= aoi.bbox[3]
                    });
                    let mut analysis = common_analysis(
                        "flares",
                        &flare_method,
                        aoi,
                        item,
                        &c.source,
                        chip.footprint(),
                        "ok",
                    );
                    analysis["cloud_analysis"] = cloud_ref.clone().into();
                    flare_glint(&mut analysis, item);
                    match commit(
                        root,
                        aoi,
                        item,
                        "flares",
                        &flare_method,
                        record::collection(analysis, flare_features(&detections)),
                    ) {
                        Ok(()) => counts[2] += detections.len(),
                        Err(e) => {
                            counts[4] += 1;
                            eprintln!("    {} flare record FAIL: {e}", item.id);
                        }
                    }
                }
            }

            if want_plumes && !plumes_done {
                let result = plume_detector.as_ref().unwrap().detect(
                    item,
                    &candidates,
                    lon,
                    lat,
                    &mut cache,
                );
                match result {
                    Ok((plume_chip, result)) => {
                        let probability =
                            if result.is_plume() {
                                let asset = root.join("assets").join(&aoi.key).join(&item.id).join(
                                    format!(
                                        "plumes-{}.tif",
                                        plume_method["fingerprint"].as_str().unwrap()
                                    ),
                                );
                                match plume::save_probability(&asset, &plume_chip, &result) {
                                    Ok(()) => Some(
                                        asset
                                            .strip_prefix(root)
                                            .unwrap()
                                            .to_string_lossy()
                                            .into_owned(),
                                    ),
                                    Err(e) => {
                                        counts[4] += 1;
                                        record::persist_error(
                                            &record::error_path(root, &aoi.key, &item.id, "plumes"),
                                            &e,
                                        );
                                        eprintln!("    {} probability FAIL: {e}", item.id);
                                        continue;
                                    }
                                }
                            } else {
                                None
                            };
                        let mut analysis = common_analysis(
                            "plumes",
                            &plume_method,
                            aoi,
                            item,
                            &c.source,
                            plume_chip.footprint(),
                            &result.status,
                        );
                        analysis["cloud_analysis"] = cloud_ref.clone().into();
                        analysis["clear_percent"] = json!(result.clear_percent);
                        analysis["background_scene"] = json!(result.background);
                        analysis["wind"] = json!(result.wind);
                        analysis["scene_score"] = json!(result.scene_score);
                        if let Some(asset) = probability {
                            analysis["assets"] = json!({"probability": asset});
                        }
                        let n = result.plumes.len();
                        match commit(
                            root,
                            aoi,
                            item,
                            "plumes",
                            &plume_method,
                            record::collection(analysis, plume_features(&plume_chip, &result)),
                        ) {
                            Ok(()) => counts[3] += n,
                            Err(e) => {
                                counts[4] += 1;
                                eprintln!("    {} plume record FAIL: {e}", item.id);
                            }
                        }
                    }
                    Err(e) => {
                        counts[4] += 1;
                        record::persist_error(
                            &record::error_path(root, &aoi.key, &item.id, "plumes"),
                            &e,
                        );
                        eprintln!("    {} plume FAIL: {e}", item.id);
                    }
                }
            }
            eprintln!("    {}: committed detector records", item.id);
        }
    }
    if counts[4] > 0 {
        eprintln!(
            "incomplete: {} error(s); rerun resumes by analysis fingerprint",
            counts[4]
        );
        std::process::exit(1);
    }
    eprintln!(
        "done: {} acquisitions · {} cached · {} flare(s) · {} plume(s) -> {out}/observations",
        counts[0], counts[1], counts[2], counts[3]
    );
}

pub fn run_flares(c: &Common, out: &str, pool: &rayon::ThreadPool) {
    let root = Path::new(out);
    let thresholds = c.thresholds();
    let flare_method = flare_method(
        &thresholds,
        if c.source.ends_with("l1c") {
            "l1c-toa"
        } else {
            "l2a-surface"
        },
    );
    let cloud_method = scl_method();
    let aois = super::load_aois(c);
    let (start, end) = c.dates();
    let reader = read::make_reader(c.gpu, c.region.is_some()).unwrap_or_else(|e| super::die(&e));
    let mut totals = [0usize; 4]; // scenes, cached, detections, errors
    eprintln!(
        "detect: flares · {} AOI(s) · {start} -> {end} · canonical GeoJSON",
        aois.len()
    );
    for aoi in &aois {
        let mut items = match stac::search(aoi.bbox, &start, &end, c.cloud, &c.source) {
            Ok(items) => items,
            Err(e) => {
                totals[3] += 1;
                eprintln!("  {} search FAIL: {e}", aoi.id);
                continue;
            }
        };
        super::filter_tiles(c, &mut items);
        totals[0] += items.len();
        let results = pool.install(|| {
            items
                .par_iter()
                .map(|item| {
                    if complete(root, aoi, item, "flares", &flare_method) {
                        return (1usize, 0usize, 0usize);
                    }
                    let error = record::error_path(root, &aoi.key, &item.id, "flares");
                    let detections = match read::detect_scene(
                        &*reader,
                        item,
                        super::det_bbox(aoi, item),
                        aoi.full_tile,
                        &thresholds,
                        false,
                    ) {
                        Ok((detections, _)) => detections,
                        Err(e) => {
                            record::persist_error(&error, &e);
                            eprintln!("  {} {} FAIL: {e}", aoi.id, item.id);
                            return (0, 0, 1);
                        }
                    };
                    let cells = read::cloud_scene(item, super::det_bbox(aoi, item), aoi.full_tile);
                    let cloud = write_clouds(
                        root,
                        c,
                        aoi,
                        item,
                        CloudRecord {
                            method: &cloud_method,
                            footprint: record::bbox_geometry(super::det_bbox(aoi, item)),
                            status: if item.bands.scl.is_some() {
                                "ok"
                            } else {
                                "unavailable"
                            },
                            cells: &cells,
                        },
                    );
                    let cloud_path = match cloud {
                        Ok(path) => path,
                        Err(e) => {
                            record::persist_error(&error, &e);
                            return (0, 0, 1);
                        }
                    };
                    let mut analysis = common_analysis(
                        "flares",
                        &flare_method,
                        aoi,
                        item,
                        &c.source,
                        record::bbox_geometry(super::det_bbox(aoi, item)),
                        "ok",
                    );
                    analysis["cloud_analysis"] = relative_record_name(&cloud_path).into();
                    flare_glint(&mut analysis, item);
                    match commit(
                        root,
                        aoi,
                        item,
                        "flares",
                        &flare_method,
                        record::collection(analysis, flare_features(&detections)),
                    ) {
                        Ok(()) => (0, detections.len(), 0),
                        Err(e) => {
                            record::persist_error(&error, &e);
                            (0, 0, 1)
                        }
                    }
                })
                .reduce(|| (0, 0, 0), |a, b| (a.0 + b.0, a.1 + b.1, a.2 + b.2))
        });
        totals[1] += results.0;
        totals[2] += results.1;
        totals[3] += results.2;
    }
    if totals[3] > 0 {
        eprintln!(
            "incomplete: {} error(s); rerun resumes by analysis fingerprint",
            totals[3]
        );
        std::process::exit(1);
    }
    eprintln!(
        "done: {} scenes · {} cached · {} flare detections -> {out}/observations",
        totals[0], totals[1], totals[2]
    );
}
