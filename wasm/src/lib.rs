//! wasm-bindgen shim: the frozen methodology core (detect + cluster + score)
//! exposed to js. i/o stays js glue (browser fetch byte-ranges → typed arrays in);
//! only the compute crosses. the web map clusters raw detections it pulls off the
//! archive with the SAME code the cli runs — no second clustering implementation.

use s2_flares_core::{
    cluster_detections, detect_block, BlockMeta, ClusterOptions, Detection, Thresholds,
};
use wasm_bindgen::prelude::*;

#[derive(serde::Serialize)]
struct DetectResult {
    detections: Vec<Detection>,
    cloud_free: bool,
}

/// run the block detector on typed arrays. b8a/scl are optional (pass null/undefined).
/// `meta` is a BlockMeta-shaped object; `thresholds` is an optional partial
/// `{ b12_min?, b11_min?, contrast_ratio?, … }` — omitted fields keep the
/// recall-first defaults (the full spectral mask, morphological gates neutralised).
#[wasm_bindgen(js_name = detectBlock)]
pub fn detect_block_js(
    b12: &[u16],
    b11: &[u16],
    b8a: Option<Vec<u16>>,
    scl: Option<Vec<u8>>,
    meta: JsValue,
    thresholds: JsValue,
) -> Result<JsValue, JsValue> {
    let meta: BlockMeta = serde_wasm_bindgen::from_value(meta)?;
    let t: Thresholds = if thresholds.is_undefined() || thresholds.is_null() {
        Thresholds::default()
    } else {
        serde_wasm_bindgen::from_value(thresholds)?
    };
    let (detections, cloud_free) =
        detect_block(b12, b11, b8a.as_deref(), scl.as_deref(), &meta, &t);
    Ok(serde_wasm_bindgen::to_value(&DetectResult {
        detections,
        cloud_free,
    })?)
}

// cluster options mirror lib/cluster.js defaults (map-friendly: minDates 1).
#[derive(serde::Deserialize)]
#[serde(default)]
struct Opts {
    merge_distance: f64,
    min_dates: usize,
    min_avg_b12: f64,
    observations: Option<usize>,
    score_threshold: f64,
}
impl Default for Opts {
    fn default() -> Self {
        Opts {
            merge_distance: 135.0,
            min_dates: 1,
            min_avg_b12: 0.5,
            observations: None,
            score_threshold: 0.0,
        }
    }
}

/// cluster an array of (possibly partial) detection objects into scored sites.
/// `opts` is an optional `{ merge_distance?, min_dates?, min_avg_b12?,
/// observations?, score_threshold? }`.
#[wasm_bindgen(js_name = cluster)]
pub fn cluster_js(detections: JsValue, opts: JsValue) -> Result<JsValue, JsValue> {
    let dets: Vec<Detection> = serde_wasm_bindgen::from_value(detections)?;
    let o: Opts = if opts.is_undefined() || opts.is_null() {
        Opts::default()
    } else {
        serde_wasm_bindgen::from_value(opts)?
    };
    let clusters = cluster_detections(
        &dets,
        &ClusterOptions {
            merge_distance: o.merge_distance,
            min_dates: o.min_dates,
            min_avg_b12: o.min_avg_b12,
            observations: o.observations,
            score_threshold: o.score_threshold,
        },
    );
    Ok(serde_wasm_bindgen::to_value(&clusters)?)
}

/// the vision-validated quality score for one cluster's aggregates (for callers
/// that score without re-clustering) → [ratio_score, persistence_score,
/// glint_penalty, total_score].
#[wasm_bindgen(js_name = scoreCluster)]
pub fn score_cluster_js(
    max_ratio: Option<f64>,
    n_dates: f64,
    n_obs: f64,
    min_glint: Option<f64>,
) -> Vec<f64> {
    let s = s2_flares_core::score_cluster(max_ratio, n_dates, n_obs, min_glint);
    vec![
        s.ratio_score,
        s.persistence_score,
        s.glint_penalty,
        s.total_score,
    ]
}
