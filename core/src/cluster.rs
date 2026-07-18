//! cross-date spatial clustering into persistent flare sites. 1:1 port of
//! lib/cluster.js. each cluster carries TWO complementary glint signals: the
//! spectral discriminator (median_b12_b11_ratio / likely_glint) and the
//! vision-validated quality score (score.rs, whose geometric min_glint keys off
//! sun elevation). pure function, no global state, deterministic cluster `id`.

use crate::detect::Detection;
use crate::score::{glint_score_from_elevation, glint_suspect, score_cluster};
use std::collections::HashMap;

const DEG_TO_RAD: f64 = std::f64::consts::PI / 180.0;
const R_EARTH: f64 = 6371000.0;
// median peak b12/b11 must exceed this to count as thermal (vs spectrally-flat glint).
const GLINT_B12_B11_RATIO: f64 = 1.25;

/// deterministic cluster id: base36 of a 32-bit string hash of the anchor coords
/// rounded to 4 dp. matches js `clusterHash` byte-for-byte.
fn cluster_hash(lat: f64, lon: f64) -> String {
    let s = format!("{:.4},{:.4}", lat, lon);
    let mut h: i32 = 0;
    for b in s.bytes() {
        h = h.wrapping_shl(5).wrapping_sub(h).wrapping_add(b as i32);
    }
    to_base36(h as u32)
}

fn to_base36(mut n: u32) -> String {
    if n == 0 {
        return "0".into();
    }
    const D: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let mut out = Vec::new();
    while n > 0 {
        out.push(D[(n % 36) as usize]);
        n /= 36;
    }
    out.reverse();
    String::from_utf8(out).unwrap()
}

fn fast_dist_m(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    let d_lat = (lat2 - lat1) * DEG_TO_RAD;
    let d_lon = (lon2 - lon1) * DEG_TO_RAD * (((lat1 + lat2) * 0.5) * DEG_TO_RAD).cos();
    R_EARTH * (d_lat * d_lat + d_lon * d_lon).sqrt()
}

/// all detection dates fall within april–august (months 3–7, 0-indexed)?
fn is_seasonal<'a>(dates: impl Iterator<Item = &'a str>) -> bool {
    for d in dates {
        let m: i32 = d.get(5..7).and_then(|s| s.parse().ok()).unwrap_or(1) - 1;
        if !(3..=7).contains(&m) {
            return false;
        }
    }
    true
}

/// one deduped per-date detection carried on a cluster.
#[derive(Clone, Debug, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct DedupedDet {
    pub date: String,
    pub max_b12: f64,
    pub peak_b11: Option<f64>,
    pub pixels: u32,
    pub radiance: f64,
    pub sun_elevation: Option<f64>,
    pub sun_azimuth: Option<f64>,
    pub lon: f64,
    pub lat: f64,
}

/// a persistent flare site.
#[derive(Clone, Debug, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Cluster {
    pub id: String,
    /// anchor detection's mgrs tile — the view's partition key (clusters/mgrs=…/),
    /// mirroring the detection archive. a cluster lives in one tile (its anchor's).
    pub mgrs: String,
    pub lon: f64,
    pub lat: f64,
    pub max_b12: f64,
    pub avg_b12: f64,
    /// median per-date hot-core radiance — a representative flare-volume proxy for
    /// the site (the full per-date series is on `detections`).
    pub radiance: f64,
    pub detection_count: u32,
    pub date_count: u32,
    pub first_date: String,
    pub last_date: String,
    pub persistence: Option<f64>,
    pub seasonal: bool,
    pub median_b12_b11_ratio: Option<f64>,
    pub min_sun_elevation: Option<f64>,
    pub likely_glint: Option<bool>,
    pub ratio_score: f64,
    pub persistence_score: f64,
    pub glint_penalty: f64,
    pub total_score: f64,
    pub max_ratio: Option<f64>,
    pub min_glint: Option<f64>,
    pub glint_suspect: bool,
    pub detections: Vec<DedupedDet>,
}

impl Cluster {
    /// re-attach measured clear-sky persistence and rescore. `n_clear_obs` is the
    /// number of cloud-free looks at the site (the honest denominator behind
    /// persistence = n_dates / n_clear_obs); clamped ≥ date_count so persistence ∈
    /// (0,1]. single-sources the score formula (score_cluster) so the archive path
    /// matches the fresh-detect path — no second scoring implementation to drift.
    pub fn set_observations(&mut self, n_clear_obs: usize) {
        let n = n_clear_obs.max(self.date_count as usize);
        self.persistence = if n > 0 {
            Some(self.date_count as f64 / n as f64)
        } else {
            None
        };
        let sc = score_cluster(
            self.max_ratio,
            self.date_count as f64,
            n as f64,
            self.min_glint,
        );
        self.ratio_score = sc.ratio_score;
        self.persistence_score = sc.persistence_score;
        self.glint_penalty = sc.glint_penalty;
        self.total_score = sc.total_score;
    }
}

#[derive(Clone, Copy, Debug)]
pub struct ClusterOptions {
    pub merge_distance: f64,
    pub min_dates: usize,
    pub min_avg_b12: f64,
    /// cloud-free observation count (the persistence denominator). None = no
    /// observations supplied → persistence field left null, persistence_score 0.
    pub observations: Option<usize>,
    pub score_threshold: f64,
}

impl Default for ClusterOptions {
    fn default() -> Self {
        ClusterOptions {
            merge_distance: 135.0,
            min_dates: 4,
            min_avg_b12: 0.85,
            observations: None,
            score_threshold: 0.0,
        }
    }
}

// the score + its aggregates for one cluster's deduped detections.
struct Scored {
    ratio_score: f64,
    persistence_score: f64,
    glint_penalty: f64,
    total_score: f64,
    max_ratio: Option<f64>,
    min_glint: Option<f64>,
    glint_suspect: bool,
}

fn score_of(deduped: &[&Detection], peak_b12: f64, cloud_free_count: usize) -> Scored {
    let _ = peak_b12; // brightness is not a score term (recall floor only)
    let mut min_glint: Option<f64> = None;
    let mut max_ratio: Option<f64> = None;
    for d in deduped {
        // glint from sun_elevation (single source of truth), else the stored score.
        let gs = glint_score_from_elevation(d.sun_elevation).or(d.glint_score);
        if let Some(g) = gs {
            if !g.is_nan() {
                min_glint = Some(min_glint.map_or(g, |m| m.min(g)));
            }
        }
        // ratio from the precomputed field (finite), else max_b12/peak_b11.
        let r = match d.b12_b11_ratio {
            Some(r) if r.is_finite() => Some(r),
            _ => match d.peak_b11 {
                Some(b) if b > 0.0 => Some(d.max_b12 / b),
                _ => None,
            },
        };
        if let Some(r) = r {
            if r.is_finite() {
                max_ratio = Some(max_ratio.map_or(r, |m| m.max(r)));
            }
        }
    }
    let n_dates = deduped.len() as f64;
    let sc = score_cluster(max_ratio, n_dates, cloud_free_count as f64, min_glint);
    Scored {
        ratio_score: sc.ratio_score,
        persistence_score: sc.persistence_score,
        glint_penalty: sc.glint_penalty,
        total_score: sc.total_score,
        max_ratio,
        min_glint,
        glint_suspect: glint_suspect(min_glint, max_ratio, n_dates),
    }
}

// median b12/b11 ratio, min sun elevation, likely_glint over a cluster's dets.
fn glint_metrics(dets: &[DedupedDet]) -> (Option<f64>, Option<f64>, Option<bool>) {
    let mut ratios: Vec<f64> = Vec::new();
    let mut min_sun = f64::INFINITY;
    let mut have_sun = false;
    for d in dets {
        if let Some(b) = d.peak_b11 {
            if b > 0.0 {
                ratios.push(d.max_b12 / b);
            }
        }
        if let Some(e) = d.sun_elevation {
            if e < min_sun {
                min_sun = e;
            }
            have_sun = true;
        }
    }
    let median = if ratios.is_empty() {
        None
    } else {
        ratios.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let mid = ratios.len() / 2;
        Some(if ratios.len() & 1 == 0 {
            (ratios[mid - 1] + ratios[mid]) / 2.0
        } else {
            ratios[mid]
        })
    };
    let min_sun_elevation = if have_sun { Some(min_sun) } else { None };
    let likely_glint = median.map(|m| m < GLINT_B12_B11_RATIO);
    (median, min_sun_elevation, likely_glint)
}

fn deduped_out(d: &Detection) -> DedupedDet {
    DedupedDet {
        date: d.date.clone(),
        max_b12: d.max_b12,
        peak_b11: d.peak_b11,
        pixels: d.pixels,
        radiance: d.radiance,
        sun_elevation: d.sun_elevation,
        sun_azimuth: d.sun_azimuth,
        lon: d.lon,
        lat: d.lat,
    }
}

// median of a slice (0.0 when empty) — the cluster's representative radiance.
fn median(xs: &[f64]) -> f64 {
    if xs.is_empty() {
        return 0.0;
    }
    let mut v = xs.to_vec();
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let m = v.len() / 2;
    if v.len() & 1 == 0 {
        (v[m - 1] + v[m]) / 2.0
    } else {
        v[m]
    }
}

/// spatially cluster cross-date detections into persistent flare sites.
pub fn cluster_detections(detections: &[Detection], opts: &ClusterOptions) -> Vec<Cluster> {
    if detections.is_empty() {
        return Vec::new();
    }
    let cloud_free_count = opts.observations.unwrap_or(0);
    let has_obs = opts.observations.is_some();

    // per-detection clusters (merge off).
    if opts.merge_distance == 0.0 {
        let mut clusters = Vec::new();
        for det in detections {
            if det.max_b12 < opts.min_avg_b12 {
                continue;
            }
            let out = deduped_out(det);
            let sc = score_of(&[det], det.max_b12, cloud_free_count);
            if opts.score_threshold > 0.0 && sc.total_score < opts.score_threshold {
                continue;
            }
            let dets = vec![out];
            let (median, min_sun, likely) = glint_metrics(&dets);
            clusters.push(Cluster {
                id: cluster_hash(det.lat, det.lon),
                mgrs: det.mgrs.clone(),
                lon: det.lon,
                lat: det.lat,
                max_b12: det.max_b12,
                avg_b12: det.max_b12,
                radiance: det.radiance,
                detection_count: 1,
                date_count: 1,
                first_date: det.date.clone(),
                last_date: det.date.clone(),
                persistence: if cloud_free_count > 0 {
                    Some(1.0 / cloud_free_count as f64)
                } else {
                    None
                },
                seasonal: is_seasonal(std::iter::once(det.date.as_str())),
                median_b12_b11_ratio: median,
                min_sun_elevation: min_sun,
                likely_glint: likely,
                ratio_score: sc.ratio_score,
                persistence_score: sc.persistence_score,
                glint_penalty: sc.glint_penalty,
                total_score: sc.total_score,
                max_ratio: sc.max_ratio,
                min_glint: sc.min_glint,
                glint_suspect: sc.glint_suspect,
                detections: dets,
            });
        }
        return clusters;
    }

    // sort by max_b12 descending so highest-intensity detections become anchors.
    let mut sorted: Vec<&Detection> = detections.iter().collect();
    sorted.sort_by(|a, b| b.max_b12.partial_cmp(&a.max_b12).unwrap());

    let cell_deg = opts.merge_distance / 111320.0;
    const KEY_SHIFT: i64 = 0x100000;
    let mut grid: HashMap<i64, Vec<usize>> = HashMap::new();
    // each cluster: (anchor sorted-index, member sorted-indices).
    let mut cluster_list: Vec<(usize, Vec<usize>)> = Vec::new();

    for si in 0..sorted.len() {
        let (lon, lat) = (sorted[si].lon, sorted[si].lat);
        let g_row = (lat / cell_deg).floor() as i64;
        let g_col = (lon / cell_deg).floor() as i64;
        let (mut best_idx, mut best_dist) = (usize::MAX, f64::INFINITY);
        for dr in -1..=1 {
            for dc in -1..=1 {
                let key = (g_row + dr) * KEY_SHIFT + (g_col + dc);
                if let Some(bucket) = grid.get(&key) {
                    for &ci in bucket {
                        let a = sorted[cluster_list[ci].0];
                        let d = fast_dist_m(lat, lon, a.lat, a.lon);
                        if d <= opts.merge_distance && d < best_dist {
                            best_dist = d;
                            best_idx = ci;
                        }
                    }
                }
            }
        }
        if best_idx != usize::MAX {
            cluster_list[best_idx].1.push(si);
        } else {
            let ci = cluster_list.len();
            cluster_list.push((si, vec![si]));
            grid.entry(g_row * KEY_SHIFT + g_col).or_default().push(ci);
        }
    }

    let mut result = Vec::new();
    for (_, members) in &cluster_list {
        // deduplicate: keep best detection per date, preserving first-seen order.
        let mut order: Vec<usize> = Vec::new();
        let mut pos: HashMap<&str, usize> = HashMap::new();
        for &si in members {
            let d = sorted[si];
            match pos.get(d.date.as_str()) {
                Some(&p) => {
                    if d.max_b12 > sorted[order[p]].max_b12 {
                        order[p] = si;
                    }
                }
                None => {
                    pos.insert(d.date.as_str(), order.len());
                    order.push(si);
                }
            }
        }
        let deduped: Vec<&Detection> = order.iter().map(|&si| sorted[si]).collect();
        if deduped.len() < opts.min_dates {
            continue;
        }

        let avg_b12 = deduped.iter().map(|d| d.max_b12).sum::<f64>() / deduped.len() as f64;
        if avg_b12 < opts.min_avg_b12 {
            continue;
        }
        let radiance = median(&deduped.iter().map(|d| d.radiance).collect::<Vec<_>>());

        // anchor = highest b12 detection (first wins on tie).
        let mut anchor = deduped[0];
        for d in &deduped {
            if d.max_b12 > anchor.max_b12 {
                anchor = d;
            }
        }

        let mut dates: Vec<&str> = deduped.iter().map(|d| d.date.as_str()).collect();
        dates.sort_unstable();
        let first_date = dates[0].to_string();
        let last_date = dates[dates.len() - 1].to_string();
        let seasonal = is_seasonal(deduped.iter().map(|d| d.date.as_str()));

        let persistence = if has_obs {
            if cloud_free_count > 0 {
                Some(deduped.len() as f64 / cloud_free_count as f64)
            } else {
                None
            }
        } else {
            None
        };

        let sc = score_of(&deduped, anchor.max_b12, cloud_free_count);
        if opts.score_threshold > 0.0 && sc.total_score < opts.score_threshold {
            continue;
        }

        let dets: Vec<DedupedDet> = deduped.iter().map(|d| deduped_out(d)).collect();
        let (median, min_sun, likely) = glint_metrics(&dets);

        result.push(Cluster {
            id: cluster_hash(anchor.lat, anchor.lon),
            mgrs: anchor.mgrs.clone(),
            lon: anchor.lon,
            lat: anchor.lat,
            max_b12: anchor.max_b12,
            avg_b12,
            radiance,
            detection_count: deduped.len() as u32,
            date_count: deduped.len() as u32,
            first_date,
            last_date,
            persistence,
            seasonal,
            median_b12_b11_ratio: median,
            min_sun_elevation: min_sun,
            likely_glint: likely,
            ratio_score: sc.ratio_score,
            persistence_score: sc.persistence_score,
            glint_penalty: sc.glint_penalty,
            total_score: sc.total_score,
            max_ratio: sc.max_ratio,
            min_glint: sc.min_glint,
            glint_suspect: sc.glint_suspect,
            detections: dets,
        });
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn det(date: &str, max_b12: f64, peak_b11: f64, sun: f64) -> Detection {
        Detection {
            date: date.into(),
            max_b12,
            peak_b11: Some(peak_b11),
            sun_elevation: Some(sun),
            sun_azimuth: Some(150.0),
            pixels: 1,
            lon: -102.0,
            lat: 32.0,
            ..Default::default()
        }
    }
    fn opts() -> ClusterOptions {
        ClusterOptions {
            min_dates: 4,
            min_avg_b12: 0.5,
            ..Default::default()
        }
    }

    // glint.test.mjs [1] — saturating glint
    #[test]
    fn glint_cluster() {
        let mut dets = Vec::new();
        for m in ["05", "06", "07", "08"] {
            for d in ["05", "15", "25"] {
                dets.push(det(&format!("2024-{m}-{d}"), 1.41, 1.40, 65.0));
            }
        }
        let c = &cluster_detections(&dets, &opts())[0];
        assert_eq!(c.detections[0].peak_b11, Some(1.40));
        assert!((c.median_b12_b11_ratio.unwrap() - 1.41 / 1.40).abs() < 1e-6);
        assert_eq!(c.min_sun_elevation, Some(65.0));
        assert_eq!(c.likely_glint, Some(true));
    }

    // glint.test.mjs [2] — real flare thermal ratio
    #[test]
    fn real_flare() {
        let mut dets = Vec::new();
        for m in ["05", "06", "07", "08"] {
            for d in ["05", "15", "25"] {
                dets.push(det(&format!("2024-{m}-{d}"), 0.9, 0.4, 65.0));
            }
        }
        let c = &cluster_detections(&dets, &opts())[0];
        assert!(c.median_b12_b11_ratio.unwrap() > 1.5);
        assert_eq!(c.likely_glint, Some(false));
    }

    // glint.test.mjs [4] — legacy detections, no b11
    #[test]
    fn legacy_null() {
        let dets: Vec<Detection> = ["2024-05-01", "2024-05-11", "2024-05-21", "2024-05-31"]
            .iter()
            .map(|d| Detection {
                date: (*d).into(),
                max_b12: 1.0,
                pixels: 1,
                lon: -102.0,
                lat: 32.0,
                ..Default::default()
            })
            .collect();
        let c = &cluster_detections(&dets, &opts())[0];
        assert_eq!(c.median_b12_b11_ratio, None);
        assert_eq!(c.min_sun_elevation, None);
        assert_eq!(c.likely_glint, None);
    }

    // glint.test.mjs [5]/[6] — borderline either side of 1.25
    #[test]
    fn borderline() {
        let mk = |b11: f64| -> Vec<Detection> {
            ["2024-05-15", "2024-06-15", "2024-07-15", "2024-08-15"]
                .iter()
                .map(|d| det(d, 1.0, b11, 65.0))
                .collect()
        };
        assert_eq!(
            cluster_detections(&mk(0.78), &opts())[0].likely_glint,
            Some(false)
        );
        assert_eq!(
            cluster_detections(&mk(0.83), &opts())[0].likely_glint,
            Some(true)
        );
    }

    // score.test.mjs [7] — score components attached
    #[test]
    fn score_attached() {
        let mut dets = Vec::new();
        for m in ["05", "06", "07", "08"] {
            for d in ["05", "15", "25"] {
                let mut x = det(&format!("2024-{m}-{d}"), 0.9, 0.4, 70.0);
                x.b12_b11_ratio = Some(2.25);
                dets.push(x);
            }
        }
        let o = ClusterOptions {
            min_dates: 4,
            min_avg_b12: 0.5,
            observations: Some(12),
            ..Default::default()
        };
        let c = &cluster_detections(&dets, &o)[0];
        assert_eq!(c.ratio_score, 1.0);
        assert!((c.persistence_score - 1.0).abs() < 1e-9);
        assert!((c.max_ratio.unwrap() - 2.25).abs() < 1e-9);
        assert!(c.total_score > 0.0 && c.total_score <= 0.9);
        assert_eq!(c.likely_glint, Some(false));
    }

    // set_observations attaches the measured clear-sky denominator and rescores in
    // step with score_cluster (the archive coverage path == the fresh-detect path).
    #[test]
    fn rescore_with_observations() {
        let mut dets = Vec::new();
        for m in ["05", "06", "07", "08"] {
            for d in ["05", "15", "25"] {
                let mut x = det(&format!("2024-{m}-{d}"), 0.9, 0.4, 70.0);
                x.b12_b11_ratio = Some(2.25);
                dets.push(x);
            }
        }
        // no observations supplied → persistence null, persistence_score 0.
        let mut c = cluster_detections(&dets, &opts())[0].clone();
        assert_eq!(c.persistence, None);
        assert_eq!(c.persistence_score, 0.0);
        // 12 dates over 24 clear looks → persistence 0.5; matches score_cluster directly.
        c.set_observations(24);
        assert!((c.persistence.unwrap() - 0.5).abs() < 1e-9);
        let want = crate::score::score_cluster(c.max_ratio, c.date_count as f64, 24.0, c.min_glint);
        assert!((c.persistence_score - want.persistence_score).abs() < 1e-9);
        assert!((c.total_score - want.total_score).abs() < 1e-9);
        // n_clear_obs clamped ≥ date_count → persistence ≤ 1.
        c.set_observations(1);
        assert!((c.persistence.unwrap() - 1.0).abs() < 1e-9);
    }

    // score.test.mjs [8] — scoreThreshold drops low-quality clusters
    #[test]
    fn score_threshold() {
        let mut dets = Vec::new();
        for m in ["05", "06", "07", "08"] {
            for d in ["05", "15", "25"] {
                let mut x = det(&format!("2024-{m}-{d}"), 1.0, 1.0, 20.0);
                x.b12_b11_ratio = Some(1.0);
                dets.push(x);
            }
        }
        let gated = ClusterOptions {
            min_dates: 4,
            min_avg_b12: 0.5,
            score_threshold: 0.5,
            ..Default::default()
        };
        assert_eq!(cluster_detections(&dets, &gated).len(), 0);
        assert_eq!(cluster_detections(&dets, &opts()).len(), 1);
    }
}
