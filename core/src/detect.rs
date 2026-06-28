//! sentinel-2 swir flare detection — block-level core. 1:1 port of lib/detect.js.
//!
//! pure computation: typed slices in, detections out. the spectral mask (b12/b11
//! swir-hot + background contrast + nhi-swir/saturation) is the physics and always
//! runs; the morphological size gates are the tunable part. every threshold is a
//! field of `Thresholds`, not a preset — `Thresholds::default()` is the recall-first
//! baseline (full mask, size gates neutralised); shells override fields à la carte.

use std::collections::VecDeque;
use crate::geo::{utm_params, utm_to_wgs84};
use crate::score::{glint_angle_nadir, glint_score_from_angle};

pub const BLOCK_SIZE: usize = 256;
pub const BLOCK_OVERLAP: usize = 10;

/// resolved detector thresholds — every gate is a parameter, not a constant.
/// counts are f64 to mirror js numeric comparison (a gate is neutralised by
/// setting it huge). the cli/wasm shells override individual fields; everything
/// unset keeps the recall-first defaults below.
#[derive(Clone, Copy, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize), serde(default))]
pub struct Thresholds {
    pub b12_min: f64,
    pub b11_min: f64,
    pub peak_b12_min: f64,
    pub contrast_ratio: f64,
    pub background_floor: f64,
    pub peakedness_min: f64,
    pub saturation: f64,
    /// absolute b12 reflectance floor for a pixel to count as combustion-HOT — the
    /// flare hot-core boundary, independent of the loose detection mask. separates a
    /// real flare (b12 ≫ this) from a merely warm facility surface (~0.25–0.4).
    pub hot_floor: f64,
    pub max_pixels: f64,
    pub large_pixels: f64,
    pub large_b12_min: f64,
    pub warm_fraction: f64,
    pub warm_max_pixels: f64,
    pub single_pixel_min: f64,
    pub max_cloud_local: f64,
    pub cloud_free_thresh: f64,
}

impl Default for Thresholds {
    /// recall-first defaults: the full spectral mask runs (b12/b11 swir-hot +
    /// background contrast + peakedness — the physics that makes this flare
    /// detection), while the morphological size gates stay neutralised. precision
    /// is applied downstream at cluster/score time, not here. tighten any field
    /// via the cli (--b12-min, --contrast-ratio, …) when you want a leaner archive.
    fn default() -> Self {
        Thresholds {
            b12_min: 0.25, b11_min: 0.15, peak_b12_min: 0.30, contrast_ratio: 2.0,
            background_floor: 0.10, peakedness_min: 1.0, saturation: 1.0, hot_floor: 0.50, max_pixels: 100000.0,
            large_pixels: 100000.0, large_b12_min: 0.0, warm_fraction: 0.5, warm_max_pixels: 100000.0,
            single_pixel_min: 0.25, max_cloud_local: 0.95, cloud_free_thresh: 0.30,
        }
    }
}

/// geometry + scene context for one block.
#[derive(Clone, Debug, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize), serde(default))]
pub struct BlockMeta {
    pub date: String,
    pub epsg: i32,
    pub img_min_x: f64,
    pub img_max_y: f64,
    pub res_x: f64,
    pub res_y: f64,
    pub block_offset_x: usize,
    pub block_offset_y: usize,
    pub width: usize,
    pub height: usize,
    pub mgrs: String,
    pub scene: String,
    pub sun_elevation: Option<f64>,
    pub sun_azimuth: Option<f64>,
}

/// one detection — the full discriminating metric set so any downstream gate is
/// reconstructable. mirrors the js detection object (peak_b11 is main's field name).
#[derive(Clone, Debug, Default)]
// serde(default) so JS/duckdb can pass partial detections (clustering only needs a
// few fields); alias `max_b11` so the archive's column name deserialises too.
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize), serde(default))]
pub struct Detection {
    pub lon: f64,
    pub lat: f64,
    pub date: String,
    pub mgrs: String,
    pub scene: String,
    pub max_b12: f64,
    pub avg_b12: f64,
    #[cfg_attr(feature = "serde", serde(alias = "max_b11"))]
    pub peak_b11: Option<f64>,
    pub b12_b11_ratio: Option<f64>,
    pub peakedness: f64,
    /// flare HOT-CORE area: pixels contiguously above `hot_floor` grown from the
    /// peak — the flare itself, NOT the loose spectral-mask component (which floods
    /// the whole warm facility into tens of thousands of px). a volume signal.
    pub pixels: u32,
    /// integrated excess swir radiance over the hot core: Σ(b12 − background). the
    /// primary volume proxy — it folds intensity × area, and (unlike a pegged
    /// `max_b12`) keeps ranking flares whose core has saturated by their spread.
    pub radiance: f64,
    pub saturated: u8,
    pub sun_elevation: Option<f64>,
    pub sun_azimuth: Option<f64>,
    pub glint_angle: Option<f64>,
    pub glint_score: Option<f64>,
    /// canonical-block dedup bookkeeping (the i/o layer strips these).
    pub peak_img_row: i64,
    pub peak_img_col: i64,
}

pub fn dn_to_reflectance(dn: f64) -> f64 { (dn - 1000.0) / 10000.0 }

/// one block overlapping the query bbox, in image pixel coordinates.
#[derive(Clone, Copy, Debug)]
pub struct Block {
    pub br: usize,
    pub bc: usize,
    pub window: [usize; 4], // [x0, y0, x1, y1]
}

/// enumerate blocks overlapping a wgs84 bbox. 1:1 port of cog.js enumerateBlocks
/// — pure geometry (utm projection + BLOCK_SIZE/OVERLAP grid), shared by the i/o
/// layer. `img_bbox` = [min_x, min_y, max_x, max_y] (utm).
pub fn enumerate_blocks(
    img_width: usize, img_height: usize, img_bbox: [f64; 4], res_x: f64, res_y: f64,
    bbox: [f64; 4], epsg: i32,
) -> Vec<Block> {
    let [img_min_x, img_min_y, img_max_x, img_max_y] = img_bbox;
    let (zone, is_north) = utm_params(epsg);
    let sw = crate::geo::wgs84_to_utm(bbox[0], bbox[1], zone, is_north);
    let ne = crate::geo::wgs84_to_utm(bbox[2], bbox[3], zone, is_north);

    let px0 = (((sw.0.max(img_min_x)) - img_min_x) / res_x).floor().max(0.0) as usize;
    let py0 = ((img_max_y - ne.1.min(img_max_y)) / res_y).floor().max(0.0) as usize;
    let px1 = (((ne.0.min(img_max_x)) - img_min_x) / res_x).ceil().min(img_width as f64) as usize;
    let py1 = ((img_max_y - sw.1.max(img_min_y)) / res_y).ceil().min(img_height as f64) as usize;
    if px1 <= px0 || py1 <= py0 { return Vec::new(); }

    let block_row0 = py0 / BLOCK_SIZE;
    let block_row1 = py1.div_ceil(BLOCK_SIZE);
    let block_col0 = px0 / BLOCK_SIZE;
    let block_col1 = px1.div_ceil(BLOCK_SIZE);

    let mut blocks = Vec::new();
    for br in block_row0..block_row1 {
        for bc in block_col0..block_col1 {
            let x0 = (bc * BLOCK_SIZE).saturating_sub(BLOCK_OVERLAP);
            let y0 = (br * BLOCK_SIZE).saturating_sub(BLOCK_OVERLAP);
            let x1 = ((bc + 1) * BLOCK_SIZE + BLOCK_OVERLAP).min(img_width);
            let y1 = ((br + 1) * BLOCK_SIZE + BLOCK_OVERLAP).min(img_height);
            blocks.push(Block { br, bc, window: [x0, y0, x1, y1] });
        }
    }
    blocks
}

/// cloud fraction from raw scl — (skip, cloud_free).
pub fn screen_clouds(scl: &[u8], max_cloud_local: f64, cloud_free_thresh: f64) -> (bool, bool) {
    let total = scl.len();
    if total == 0 { return (false, true); }
    let cloud = scl.iter().filter(|&&v| v == 3 || v == 8 || v == 9 || v == 10).count();
    let frac = cloud as f64 / total as f64;
    if frac > max_cloud_local { (true, false) } else { (false, frac <= cloud_free_thresh) }
}

/// 4-connected connected components over a boolean mask. seed order = raster scan
/// (i ascending), neighbour push order up/down/left/right — matches lib/detect.js.
pub fn label_connected_components(mask: &[u8], width: usize, height: usize) -> (Vec<i32>, i32) {
    let mut labels = vec![0i32; width * height];
    let mut next = 1i32;
    for i in 0..mask.len() {
        if mask[i] == 0 || labels[i] != 0 { continue; }
        let mut q = VecDeque::new();
        q.push_back(i);
        labels[i] = next;
        while let Some(idx) = q.pop_front() {
            let r = idx / width;
            let c = idx % width;
            let visit = |n: usize, q: &mut VecDeque<usize>, labels: &mut [i32]| {
                if mask[n] != 0 && labels[n] == 0 { labels[n] = next; q.push_back(n); }
            };
            if r > 0 { visit(idx - width, &mut q, &mut labels); }
            if r < height - 1 { visit(idx + width, &mut q, &mut labels); }
            if c > 0 { visit(idx - 1, &mut q, &mut labels); }
            if c < width - 1 { visit(idx + 1, &mut q, &mut labels); }
        }
        next += 1;
    }
    (labels, next - 1)
}

// the flare HOT CORE: within THIS detection's connected component (`label_id`), the
// pixels contiguously above `hot_thresh` grown from the peak (always counting the
// peak itself, so ≥1 px), capped at `cap`. returns its pixel count AND its integrated
// excess radiance Σ(b12 − bg). this measures the flare, decoupled from the loose
// spectral-mask component — which 4-connects the peak across the entire warm
// facility/glint field into a meaningless count. the component restriction stops a
// dim glint peak's core from leaking into an adjacent flare's bright pixels.
fn hot_core(b12: &[f64], labels: &[i32], label_id: i32, peak_idx: usize, hot_thresh: f64, bg: f64, w: usize, h: usize, cap: usize) -> (usize, f64) {
    let mut visited = vec![false; b12.len()];
    let mut q = VecDeque::new();
    q.push_back(peak_idx);
    visited[peak_idx] = true;
    let (mut size, mut rad) = (0usize, 0f64);
    while let Some(idx) = q.pop_front() {
        size += 1;
        rad += (b12[idx] - bg).max(0.0);
        if size > cap { return (size, rad); }
        let r = idx / w;
        let c = idx % w;
        let visit = |n: usize, q: &mut VecDeque<usize>, visited: &mut [bool]| {
            if !visited[n] && labels[n] == label_id && b12[n] > hot_thresh { visited[n] = true; q.push_back(n); }
        };
        if r > 0 { visit(idx - w, &mut q, &mut visited); }
        if r < h - 1 { visit(idx + w, &mut q, &mut visited); }
        if c > 0 { visit(idx - 1, &mut q, &mut visited); }
        if c < w - 1 { visit(idx + 1, &mut q, &mut visited); }
    }
    (size, rad)
}

/// run swir flare detection on one block → (detections, cloud_free).
pub fn detect_block(
    b12_raw: &[u16],
    b11_raw: &[u16],
    b8a_raw: Option<&[u16]>,
    scl_raw: Option<&[u8]>,
    meta: &BlockMeta,
    t: &Thresholds,
) -> (Vec<Detection>, bool) {
    let (w, h) = (meta.width, meta.height);
    if w == 0 || h == 0 { return (Vec::new(), false); }

    let mut block_cloud_free = true;
    if let Some(scl) = scl_raw {
        let (skip, cloud_free) = screen_clouds(scl, t.max_cloud_local, t.cloud_free_thresh);
        if skip { return (Vec::new(), false); }
        block_cloud_free = cloud_free;
    }

    let n = w * h;
    let mut b12 = vec![0f64; n];
    let mut bg_pixels: Vec<f64> = Vec::new();
    for i in 0..n {
        let v = (b12_raw[i] as f64 - 1000.0) / 10000.0;
        b12[i] = v;
        if v < t.b12_min { bg_pixels.push(v); }
    }
    if bg_pixels.len() < 10 { return (Vec::new(), block_cloud_free); }
    bg_pixels.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let median_bg = bg_pixels[bg_pixels.len() / 2];
    let contrast_thresh = median_bg.max(t.background_floor) * t.contrast_ratio;

    let mut b11 = vec![0f64; n];
    let mut mask = vec![0u8; n];
    let mut any_mask = false;
    let has_b8a = b8a_raw.is_some();
    for i in 0..n {
        let b11v = (b11_raw[i] as f64 - 1000.0) / 10000.0;
        b11[i] = b11v;
        let b12v = b12[i];
        if b12v <= t.b12_min || b11v <= t.b11_min { continue; }
        if b12v <= contrast_thresh { continue; }
        if has_b8a {
            let b8av = (b8a_raw.unwrap()[i] as f64 - 1000.0) / 10000.0;
            let denom = b11v + b8av;
            let nhiswnir = if denom > 0.01 { (b11v - b8av) / denom } else { 0.0 };
            if !(nhiswnir > 0.0 || b11v > t.saturation || b12v > t.saturation) { continue; }
        } else if b11v <= t.saturation {
            continue;
        }
        mask[i] = 1;
        any_mask = true;
    }
    if !any_mask { return (Vec::new(), block_cloud_free); }

    let (labels, count) = label_connected_components(&mask, w, h);
    if count == 0 { return (Vec::new(), block_cloud_free); }

    let (x0, y0) = (meta.block_offset_x, meta.block_offset_y);
    let (x1, y1) = (x0 + w, y0 + h);
    let utm_min_x = meta.img_min_x + x0 as f64 * meta.res_x;
    let utm_min_y = meta.img_max_y - y1 as f64 * meta.res_y;
    let utm_max_x = meta.img_min_x + x1 as f64 * meta.res_x;
    let utm_max_yw = meta.img_max_y - y0 as f64 * meta.res_y;
    let (zone, is_north) = utm_params(meta.epsg);

    // glint is scene-level (one sun geometry per pass), so compute once.
    let (glint_angle, glint_score) = match meta.sun_elevation {
        Some(e) => {
            let a = glint_angle_nadir(e);
            (Some(a), Some(glint_score_from_angle(a)))
        }
        None => (None, None),
    };

    // one pass to accumulate per-label peak/sum/count, instead of rescanning the whole
    // block once per component (was O(components·n)). i ascending + strict `>` keeps the
    // same lowest-index peak tie-break the per-label scan had → byte-identical detections.
    let lc = count as usize + 1;
    let (mut counts, mut sums) = (vec![0u32; lc], vec![0f64; lc]);
    let (mut peaks, mut peak_idxs) = (vec![f64::NEG_INFINITY; lc], vec![usize::MAX; lc]);
    for i in 0..n {
        let l = labels[i] as usize;
        if l == 0 { continue; }
        counts[l] += 1;
        sums[l] += b12[i];
        if b12[i] > peaks[l] { peaks[l] = b12[i]; peak_idxs[l] = i; }
    }

    let mut detections = Vec::new();
    for label_id in 1..=count {
        let l = label_id as usize;
        let (n_pixels, peak_b12, peak_idx, sum_b12) = (counts[l], peaks[l], peak_idxs[l], sums[l]);

        // --- tunable morphological gates (LOOSE neutralises all of these) ---
        let np = n_pixels as f64;
        if np > t.max_pixels { continue; }
        if peak_b12 < t.peak_b12_min { continue; }
        if np > t.large_pixels && peak_b12 < t.large_b12_min { continue; }
        let avg_b12 = sum_b12 / np;
        if n_pixels > 1 && peak_b12 < t.peakedness_min * avg_b12 && avg_b12 < t.saturation { continue; }
        if n_pixels == 1 && peak_b12 < t.single_pixel_min { continue; }

        let peak_row = peak_idx / w;
        let peak_col = peak_idx % w;
        // the flare's hot core, not the loose component: contiguous-from-peak above
        // max(peak·warm_fraction, hot_floor). the absolute floor bounds dim diffuse
        // glint/haze fields (which a purely peak-relative threshold lets flood).
        let hot_thresh = (peak_b12 * t.warm_fraction).max(t.hot_floor);
        let (core_px, radiance) = hot_core(&b12, &labels, label_id, peak_idx, hot_thresh, median_bg, w, h, t.warm_max_pixels as usize);
        if core_px as f64 > t.warm_max_pixels { continue; }

        let col_frac = (peak_col as f64 + 0.5) / w as f64;
        let row_frac = (peak_row as f64 + 0.5) / h as f64;
        let utm_x = utm_min_x + col_frac * (utm_max_x - utm_min_x);
        let utm_y = utm_max_yw - row_frac * (utm_max_yw - utm_min_y);
        let (lon, lat) = utm_to_wgs84(utm_x, utm_y, zone, is_north);

        let peak_b11 = b11[peak_idx];
        let ratio = if peak_b11 > 1e-6 { peak_b12 / peak_b11 } else { f64::INFINITY };

        detections.push(Detection {
            lon, lat,
            date: meta.date.clone(),
            mgrs: meta.mgrs.clone(),
            scene: meta.scene.clone(),
            max_b12: peak_b12,
            avg_b12,
            peak_b11: Some(peak_b11),
            b12_b11_ratio: Some(ratio),
            peakedness: peak_b12 / avg_b12,
            pixels: core_px as u32,
            radiance,
            saturated: if peak_b12 >= t.saturation { 1 } else { 0 },
            sun_elevation: meta.sun_elevation,
            sun_azimuth: meta.sun_azimuth,
            glint_angle,
            glint_score,
            peak_img_row: (y0 + peak_row) as i64,
            peak_img_col: (x0 + peak_col) as i64,
        });
    }
    (detections, block_cloud_free)
}
