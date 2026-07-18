//! scl clear-sky coverage sampling — the pure compute half of lib/coverage.js.
//! given a full scl band (read by the i/o layer) it samples a small window at
//! every in-footprint catalogue site and emits the class histogram (the honest
//! n_clear_obs denominator behind persistence). the cloud-free rule stays a sql
//! knob downstream — we store the whole histogram.

use crate::geo::{utm_params, wgs84_to_utm};

// half-window in pixels: 2 ⇒ a 5×5 px (~100 m) box centred on the site.
const HALF_WIN: i64 = 2;
// scl has 12 classes (0–11); 0 is nodata.
pub const N_SCL: usize = 12;

// the clear-sky rule (permian sql/30): a look is clear when cloud_frac ≤ this.
pub const CLEAR_MAX: f64 = 0.10;
// the fixed ~100 m geographic grid the cloud mask snaps onto (3 dp ≈ the 5×5 @ 20 m
// window). detection samples cells on this grid; clustering snaps each anchor onto
// the SAME grid → the join hits. the ONLY hard rule is both sides snap identically.
pub const GRID_STEP: f64 = 0.001;
pub fn snap(x: f64) -> f64 {
    (x / GRID_STEP).round() * GRID_STEP
}
/// the cloud-mask join key: lon/lat snapped to the grid, formatted at the grid's 3 dp
/// (keep in step with GRID_STEP). a grid value re-snapped is idempotent, so detection
/// (grid centres) and clustering (snapped anchors) format to the same string.
pub fn cell_key(lon: f64, lat: f64) -> String {
    format!("{:.3},{:.3}", snap(lon), snap(lat))
}

/// grid of cell-centre sites tiling `bbox` [w,s,e,n] at GRID_STEP, each h3 its
/// `cell_key` — the cloud-mask sampling sites for one scan window.
pub fn grid_sites(bbox: [f64; 4]) -> Vec<Site> {
    let [w, s, e, n] = bbox;
    let k = |x: f64| (snap(x) / GRID_STEP).round() as i64;
    let mut sites = Vec::new();
    for j in k(s)..=k(n) {
        let lat = j as f64 * GRID_STEP;
        for i in k(w)..=k(e) {
            let lon = i as f64 * GRID_STEP;
            sites.push(Site {
                h3: cell_key(lon, lat),
                lon,
                lat,
            });
        }
    }
    sites
}

#[derive(Clone, Debug)]
pub struct Site {
    pub h3: String,
    pub lon: f64,
    pub lat: f64,
}

#[derive(Clone, Debug)]
pub struct CoverRow {
    pub h3: String,
    pub px_valid: u32,
    pub hist: [u32; N_SCL],
}

impl CoverRow {
    /// cloud fraction = (shadow + cloud med + cloud high + cirrus) / valid px — the
    /// single clear-sky classifier shared by the detection cloud mask and the cluster
    /// join, so the two can't drift. `cloud_frac ≤ CLEAR_MAX` ⇒ a clear look.
    pub fn cloud_frac(&self) -> f64 {
        if self.px_valid == 0 {
            return 0.0;
        }
        (self.hist[3] + self.hist[8] + self.hist[9] + self.hist[10]) as f64 / self.px_valid as f64
    }
}

/// sample the scl band at each in-footprint site. `img_bbox` = [min_x, min_y,
/// max_x, max_y] (utm); width/height in pixels; epsg the tile's utm code.
pub fn cover_sites(
    scl: &[u8],
    width: usize,
    height: usize,
    img_bbox: [f64; 4],
    epsg: i32,
    sites: &[Site],
) -> Vec<CoverRow> {
    let [min_x, min_y, max_x, max_y] = img_bbox;
    let res_x = (max_x - min_x) / width as f64;
    let res_y = (max_y - min_y) / height as f64;
    let (zone, is_north) = utm_params(epsg);
    let (w, h) = (width as i64, height as i64);

    let mut rows = Vec::new();
    for s in sites {
        let (easting, northing) = wgs84_to_utm(s.lon, s.lat, zone, is_north);
        let cx = ((easting - min_x) / res_x).floor() as i64;
        let cy = ((max_y - northing) / res_y).floor() as i64;
        if cx < 0 || cy < 0 || cx >= w || cy >= h {
            continue;
        }

        let mut hist = [0u32; N_SCL];
        let mut valid = 0u32;
        for dy in -HALF_WIN..=HALF_WIN {
            let y = cy + dy;
            if y < 0 || y >= h {
                continue;
            }
            let row = (y * w) as usize;
            for dx in -HALF_WIN..=HALF_WIN {
                let x = cx + dx;
                if x < 0 || x >= w {
                    continue;
                }
                let v = scl[row + x as usize] as usize;
                if v < N_SCL {
                    hist[v] += 1;
                    if v != 0 {
                        valid += 1;
                    }
                }
            }
        }
        if valid == 0 {
            continue;
        }
        rows.push(CoverRow {
            h3: s.h3.clone(),
            px_valid: valid,
            hist,
        });
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geo::{utm_params, wgs84_to_utm};

    // a site at a tile's centre samples its 5×5 (HALF_WIN=2) window; a site off the
    // tile is dropped (in-footprint filter); class 0 (nodata) doesn't count as valid.
    #[test]
    fn samples_site_window() {
        let (lon, lat, epsg) = (-102.0, 32.0, 32613); // utm 13N
        let (zone, is_north) = utm_params(epsg);
        let (e, n) = wgs84_to_utm(lon, lat, zone, is_north);
        let (w, h) = (20usize, 20usize);
        let bbox = [e - 100.0, n - 100.0, e + 100.0, n + 100.0]; // 10 m px, centre at (10,10)
        let mut scl = vec![6u8; w * h]; // all water (valid, not cloud)
        scl[0] = 0; // a nodata pixel outside the central window — must not be counted
        let sites = vec![
            Site {
                h3: "centre".into(),
                lon,
                lat,
            },
            Site {
                h3: "offtile".into(),
                lon: lon + 1.0,
                lat,
            },
        ];
        let rows = cover_sites(&scl, w, h, bbox, epsg, &sites);
        assert_eq!(rows.len(), 1, "off-tile site dropped");
        assert_eq!(rows[0].h3, "centre");
        assert_eq!(rows[0].px_valid, 25); // full 5×5, none nodata
        assert_eq!(rows[0].hist[6], 25);
        assert_eq!(rows[0].hist[0], 0);
    }

    // the grid tiles the bbox at GRID_STEP, cell keys are snapped+stable, and an
    // anchor inside a cell snaps back to that cell's key (the join contract).
    #[test]
    fn grid_and_key() {
        let sites = grid_sites([51.000, 25.000, 51.002, 25.001]); // 3 lon × 2 lat
        assert_eq!(sites.len(), 6);
        assert_eq!(sites[0].h3, "51.000,25.000");
        // an anchor 0.3 cell off a grid centre snaps to that centre's key.
        assert_eq!(cell_key(51.0013, 25.0004), "51.001,25.000");
        // the classifier: 4 cloud/shadow of 25 valid → 0.16 (not clear).
        let mut hist = [0u32; N_SCL];
        hist[6] = 21;
        hist[3] = 1;
        hist[8] = 1;
        hist[9] = 1;
        hist[10] = 1;
        let r = CoverRow {
            h3: "x".into(),
            px_valid: 25,
            hist,
        };
        assert!((r.cloud_frac() - 0.16).abs() < 1e-9);
        assert!(r.cloud_frac() > CLEAR_MAX);
    }
}
