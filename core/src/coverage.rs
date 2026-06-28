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

/// sample the scl band at each in-footprint site. `img_bbox` = [min_x, min_y,
/// max_x, max_y] (utm); width/height in pixels; epsg the tile's utm code.
pub fn cover_sites(scl: &[u8], width: usize, height: usize, img_bbox: [f64; 4], epsg: i32, sites: &[Site]) -> Vec<CoverRow> {
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
        if cx < 0 || cy < 0 || cx >= w || cy >= h { continue; }

        let mut hist = [0u32; N_SCL];
        let mut valid = 0u32;
        for dy in -HALF_WIN..=HALF_WIN {
            let y = cy + dy;
            if y < 0 || y >= h { continue; }
            let row = (y * w) as usize;
            for dx in -HALF_WIN..=HALF_WIN {
                let x = cx + dx;
                if x < 0 || x >= w { continue; }
                let v = scl[row + x as usize] as usize;
                if v < N_SCL {
                    hist[v] += 1;
                    if v != 0 { valid += 1; }
                }
            }
        }
        if valid == 0 { continue; }
        rows.push(CoverRow { h3: s.h3.clone(), px_valid: valid, hist });
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
            Site { h3: "centre".into(), lon, lat },
            Site { h3: "offtile".into(), lon: lon + 1.0, lat },
        ];
        let rows = cover_sites(&scl, w, h, bbox, epsg, &sites);
        assert_eq!(rows.len(), 1, "off-tile site dropped");
        assert_eq!(rows[0].h3, "centre");
        assert_eq!(rows[0].px_valid, 25); // full 5×5, none nodata
        assert_eq!(rows[0].hist[6], 25);
        assert_eq!(rows[0].hist[0], 0);
    }
}
