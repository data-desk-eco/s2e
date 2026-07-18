//! Sentinel-2 L1C chip ingestion, CloudSEN preparation and spatial footprints.

use super::PlumeDetection;
use crate::models::CloudModel;
use crate::read::{open, radiometry, Raster};
use crate::stac::Item;
use rayon::prelude::*;

const RESOLUTION: [usize; 13] = [60, 10, 10, 10, 20, 20, 20, 10, 20, 60, 60, 20, 20];
const INTERPOLATE_20M: [usize; 6] = [4, 5, 6, 8, 11, 12];

#[derive(Clone, Debug)]
pub struct Chip {
    /// All 13 L1C channels on the 10 m grid, channel-major, reflectance×10000.
    pub values: Vec<f32>,
    pub nearest: Vec<u16>,
    pub valid: Vec<bool>,
    pub width: usize,
    pub height: usize,
    pub min_x: f64,
    pub max_y: f64,
    pub epsg: i32,
    pub clear_percent: f32,
}

impl Chip {
    pub fn model_bands(&self) -> Vec<f32> {
        let n = self.width * self.height;
        let indices = [1usize, 2, 3, 7, 11, 12];
        let mut out = Vec::with_capacity(6 * n);
        for c in indices {
            out.extend_from_slice(&self.values[c * n..(c + 1) * n]);
        }
        out
    }

    pub fn footprint(&self) -> serde_json::Value {
        let (zone, north) = s2e_core::utm_params(self.epsg);
        let min_y = self.max_y - self.height as f64 * 10.0;
        let max_x = self.min_x + self.width as f64 * 10.0;
        let p = |x, y| {
            let (lon, lat) = s2e_core::utm_to_wgs84(x, y, zone, north);
            vec![lon, lat]
        };
        serde_json::json!({
            "type": "Polygon",
            "coordinates": [[
                p(self.min_x, min_y), p(max_x, min_y), p(max_x, self.max_y),
                p(self.min_x, self.max_y), p(self.min_x, min_y)
            ]]
        })
    }
}

/// A compact auditable footprint for one connected plume component. The
/// probability GeoTIFF remains the pixel-exact asset; GeoJSON carries the
/// component's georeferenced envelope for indexing and quick CLI use.
pub fn component_geometry(chip: &Chip, plume: &PlumeDetection) -> serde_json::Value {
    let mut min_col = usize::MAX;
    let mut min_row = usize::MAX;
    let mut max_col = 0usize;
    let mut max_row = 0usize;
    for (i, &on) in plume.mask.iter().enumerate() {
        if on == 0 {
            continue;
        }
        let (row, col) = (i / chip.width, i % chip.width);
        min_col = min_col.min(col);
        max_col = max_col.max(col + 1);
        min_row = min_row.min(row);
        max_row = max_row.max(row + 1);
    }
    if min_col == usize::MAX {
        return serde_json::Value::Null;
    }
    let (zone, north) = s2e_core::utm_params(chip.epsg);
    let (x0, x1) = (
        chip.min_x + min_col as f64 * 10.0,
        chip.min_x + max_col as f64 * 10.0,
    );
    let (y0, y1) = (
        chip.max_y - max_row as f64 * 10.0,
        chip.max_y - min_row as f64 * 10.0,
    );
    let p = |x, y| {
        let (lon, lat) = s2e_core::utm_to_wgs84(x, y, zone, north);
        vec![lon, lat]
    };
    serde_json::json!({
        "type": "Polygon",
        "coordinates": [[p(x0,y0),p(x1,y0),p(x1,y1),p(x0,y1),p(x0,y0)]]
    })
}

/// CloudSEN clear-sky cells on the same 0.001° key grid used by flare
/// persistence. This lets the combined L1C pass supply the denominator without
/// a second SCL/L2A read.
pub fn cloud_cells(chip: &Chip) -> Vec<(String, f64)> {
    let (zone, north) = s2e_core::utm_params(chip.epsg);
    let min_y = chip.max_y - chip.height as f64 * 10.0;
    let max_x = chip.min_x + chip.width as f64 * 10.0;
    let corners = [
        s2e_core::utm_to_wgs84(chip.min_x, min_y, zone, north),
        s2e_core::utm_to_wgs84(chip.min_x, chip.max_y, zone, north),
        s2e_core::utm_to_wgs84(max_x, min_y, zone, north),
        s2e_core::utm_to_wgs84(max_x, chip.max_y, zone, north),
    ];
    let bbox = [
        corners.iter().map(|p| p.0).fold(f64::INFINITY, f64::min),
        corners.iter().map(|p| p.1).fold(f64::INFINITY, f64::min),
        corners
            .iter()
            .map(|p| p.0)
            .fold(f64::NEG_INFINITY, f64::max),
        corners
            .iter()
            .map(|p| p.1)
            .fold(f64::NEG_INFINITY, f64::max),
    ];
    let n = chip.width * chip.height;
    s2e_core::grid_sites(bbox)
        .into_iter()
        .filter_map(|site| {
            let (x, y) = s2e_core::wgs84_to_utm(site.lon, site.lat, zone, north);
            let px = ((x - chip.min_x) / 10.0).round() as isize;
            let py = ((chip.max_y - y) / 10.0).round() as isize;
            let mut observed = 0usize;
            let mut obscured = 0usize;
            for iy in (py - 5)..=(py + 5) {
                for ix in (px - 5)..=(px + 5) {
                    if ix < 0 || iy < 0 || ix >= chip.width as isize || iy >= chip.height as isize {
                        continue;
                    }
                    let i = iy as usize * chip.width + ix as usize;
                    // B02=0 is outside the observed tile, not a cloudy sample.
                    if chip.nearest[n + i] == 0 {
                        continue;
                    }
                    observed += 1;
                    obscured += usize::from(!chip.valid[i]);
                }
            }
            (observed > 0).then(|| (site.h3, obscured as f64 / observed as f64))
        })
        .collect()
}

fn urls(item: &Item) -> Result<[&str; 13], String> {
    let b = &item.bands;
    let found: Vec<&str> = [
        b.b01.as_deref(),
        b.b02.as_deref(),
        b.b03.as_deref(),
        b.b04.as_deref(),
        b.b05.as_deref(),
        b.b06.as_deref(),
        b.b07.as_deref(),
        b.b08.as_deref(),
        b.b8a.as_deref(),
        b.b09.as_deref(),
        b.b10.as_deref(),
        b.b11.as_deref(),
        b.b12.as_deref(),
    ]
    .into_iter()
    .map(|u| u.ok_or_else(|| format!("{} lacks an L1C band", item.id)))
    .collect::<Result<_, _>>()?;
    found
        .try_into()
        .map_err(|_| "expected 13 L1C bands".to_string())
}

/// Reference 2 km site footprint, snapped outward to the 60 m grid exactly as
/// the CDSE Python reader does. The resulting chip is normally 204×204.
pub fn site_bounds(lon: f64, lat: f64, epsg: i32) -> [f64; 4] {
    let (zone, north) = s2e_core::utm_params(epsg);
    let (x, y) = s2e_core::wgs84_to_utm(lon, lat, zone, north);
    [
        ((x - 1000.0) / 60.0).floor() * 60.0,
        ((y - 1000.0) / 60.0).floor() * 60.0,
        ((x + 1000.0) / 60.0).ceil() * 60.0,
        ((y + 1000.0) / 60.0).ceil() * 60.0,
    ]
}

fn band_window(
    r: &Raster,
    bounds: [f64; 4],
    resolution: usize,
    offset: f64,
) -> Result<Vec<u16>, String> {
    let out_w = ((bounds[2] - bounds[0]) / 10.0).round() as usize;
    let out_h = ((bounds[3] - bounds[1]) / 10.0).round() as usize;
    let scale = resolution / 10;
    let native_w = out_w.div_ceil(scale);
    let native_h = out_h.div_ceil(scale);
    let x0 = ((bounds[0] - r.bbox[0]) / r.res_x).round() as isize;
    let y0 = ((r.bbox[3] - bounds[3]) / r.res_y).round() as isize;
    let x1 = x0 + native_w as isize;
    let y1 = y0 + native_h as isize;
    let sx0 = x0.max(0).min(r.width as isize);
    let sy0 = y0.max(0).min(r.height as isize);
    let sx1 = x1.max(0).min(r.width as isize);
    let sy1 = y1.max(0).min(r.height as isize);
    let mut native = vec![0u16; native_w * native_h];
    if sx1 > sx0 && sy1 > sy0 {
        let w = (sx1 - sx0) as usize;
        let h = (sy1 - sy0) as usize;
        let band = r.ds.rasterband(1).map_err(|e| e.to_string())?;
        let read = band
            .read_as::<u16>((sx0, sy0), (w, h), (w, h), None)
            .map_err(|e| e.to_string())?;
        let dx = (sx0 - x0) as usize;
        let dy = (sy0 - y0) as usize;
        for row in 0..h {
            native[(dy + row) * native_w + dx..(dy + row) * native_w + dx + w]
                .copy_from_slice(&read.data()[row * w..(row + 1) * w]);
        }
    }
    let mut out = vec![0u16; out_w * out_h];
    for y in 0..out_h {
        for x in 0..out_w {
            let dn =
                native[(y / scale).min(native_h - 1) * native_w + (x / scale).min(native_w - 1)];
            out[y * out_w + x] = if dn == 0 {
                0
            } else {
                (dn as f64 + offset).max(0.0).min(u16::MAX as f64) as u16
            };
        }
    }
    Ok(out)
}

fn bilinear_20m(nearest: &[u16], width: usize, height: usize) -> Vec<f32> {
    // Recover the native samples from the nearest-expanded GEE-style image,
    // then resize with half-pixel bilinear coordinates (skimage order=1).
    let sw = width.div_ceil(2);
    let sh = height.div_ceil(2);
    let mut src = vec![0f32; sw * sh];
    for y in 0..sh {
        for x in 0..sw {
            src[y * sw + x] =
                nearest[(y * 2).min(height - 1) * width + (x * 2).min(width - 1)] as f32;
        }
    }
    let mut out = vec![0f32; width * height];
    for y in 0..height {
        let fy = (y as f32 + 0.5) * sh as f32 / height as f32 - 0.5;
        let y0 = fy.floor() as isize;
        let wy = fy - y0 as f32;
        for x in 0..width {
            let fx = (x as f32 + 0.5) * sw as f32 / width as f32 - 0.5;
            let x0 = fx.floor() as isize;
            let wx = fx - x0 as f32;
            // GeoTensor.resize delegates to skimage with its default
            // `mode="constant"` and the image fill value (zero).
            let at = |xx: isize, yy: isize| {
                if xx < 0 || yy < 0 || xx >= sw as isize || yy >= sh as isize {
                    0.0
                } else {
                    src[yy as usize * sw + xx as usize]
                }
            };
            out[y * width + x] = ((at(x0, y0) * (1.0 - wx) + at(x0 + 1, y0) * wx) * (1.0 - wy)
                + (at(x0, y0 + 1) * (1.0 - wx) + at(x0 + 1, y0 + 1) * wx) * wy)
                .round();
        }
    }
    out
}

pub(super) fn reflect_index(i: isize, n: usize) -> usize {
    if n <= 1 {
        return 0;
    }
    let period = 2 * (n - 1) as isize;
    let mut j = i % period;
    if j < 0 {
        j += period;
    }
    if j >= n as isize {
        (period - j) as usize
    } else {
        j as usize
    }
}

fn cloud_input(
    nearest: &[Vec<u16>],
    width: usize,
    height: usize,
) -> (Vec<f32>, usize, usize, [usize; 4]) {
    let pw = width.div_ceil(32) * 32;
    let ph = height.div_ceil(32) * 32;
    let left = (pw - width) / 2;
    let top = (ph - height) / 2;
    let mut out = vec![0f32; 13 * pw * ph];
    for c in 0..13 {
        for y in 0..ph {
            for x in 0..pw {
                let sx = reflect_index(x as isize - left as isize, width);
                let sy = reflect_index(y as isize - top as isize, height);
                out[c * pw * ph + y * pw + x] = nearest[c][sy * width + sx] as f32 / 10000.0;
            }
        }
    }
    (
        out,
        pw,
        ph,
        [left, top, pw - width - left, ph - height - top],
    )
}

pub fn read_chip(item: &Item, lon: f64, lat: f64, clouds: &CloudModel) -> Result<Chip, String> {
    if item.level != "l1c" {
        return Err("plume detection requires Sentinel-2 L1C".into());
    }
    let band_urls = urls(item)?;
    let bounds = site_bounds(lon, lat, item.epsg);
    let calibration = radiometry(item)?;
    let nearest: Vec<Vec<u16>> = band_urls
        .par_iter()
        .enumerate()
        .map(|(i, url)| {
            let raster = open(url)?;
            band_window(&raster, bounds, RESOLUTION[i], calibration.offset)
        })
        .collect::<Result<_, String>>()?;
    let width = ((bounds[2] - bounds[0]) / 10.0).round() as usize;
    let height = ((bounds[3] - bounds[1]) / 10.0).round() as usize;
    let n = width * height;
    if nearest.iter().any(|b| b.len() != n) {
        return Err("L1C band dimensions disagree".into());
    }

    let (cloud_tensor, pw, ph, pad) = cloud_input(&nearest, width, height);
    let padded = clouds.predict(&cloud_tensor, pw, ph)?;
    let mut classes = vec![4u8; n];
    let mut valid = vec![false; n];
    let mut clear = 0usize;
    for y in 0..height {
        for x in 0..width {
            let i = y * width + x;
            let invalid = nearest.iter().any(|b| b[i] == 0);
            let class = if invalid {
                4
            } else {
                padded[(y + pad[1]) * pw + x + pad[0]]
            };
            classes[i] = class;
            valid[i] = class == 0;
            clear += usize::from(valid[i]);
        }
    }

    let mut values = vec![0f32; 13 * n];
    for c in 0..13 {
        if INTERPOLATE_20M.contains(&c) {
            values[c * n..(c + 1) * n].copy_from_slice(&bilinear_20m(&nearest[c], width, height));
        } else {
            for i in 0..n {
                values[c * n + i] = nearest[c][i] as f32;
            }
        }
    }
    Ok(Chip {
        values,
        valid,
        width,
        height,
        min_x: bounds[0],
        max_y: bounds[3],
        epsg: item.epsg,
        nearest: nearest.into_iter().flatten().collect(),
        clear_percent: clear as f32 * 100.0 / n as f32,
    })
}
