//! gdal i/o shell — the native sibling of lib/cog-gdal.js (and lib/cog.js). reads
//! sentinel-2 bands via gdal: /vsis3/eodata jp2 (JP2OpenJPEG) on the cloudferro
//! box, or /vsicurl COG byte-ranges for the aws path. windowed reads + overview
//! decimation, then hands the exact typed slices detect.js/core expect to
//! detect_block. one reader covers both paths — gdal opens jp2 and cog alike.
//!
//! harmonisation: raw eodata jp2 carries the N0400 +1000 BOA_ADD_OFFSET (acq ≥
//! 2022-01-25) that the element84 cogs were tuned without, so the reader subtracts
//! it on spectral bands (not scl). aws cogs are pre-harmonised → no shift.

use gdal::raster::ResampleAlg;
use gdal::Dataset;
use s2_flares_core::{detect_block, enumerate_blocks, BlockMeta, Detection, Thresholds, BLOCK_SIZE};
use crate::stac::Item;

const OVERVIEW_FACTOR: usize = 8; // 8× = 160 m screen for 20 m bands

fn b12_dn_min(t: &Thresholds) -> f64 { t.b12_min * 10000.0 + 1000.0 }
fn harmonize_offset(date: &str) -> u16 { if date >= "2022-01-25" { 1000 } else { 0 } }

/// set gdal's s3 endpoint + curl cache once (idempotent). eodata creds come from
/// the env (per-vm on the box); AWS_S3_ENDPOINT overridable for cloudferro vs cdse.
pub fn configure() {
    let endpoint = std::env::var("AWS_S3_ENDPOINT").unwrap_or_else(|_| "eodata.dataspace.copernicus.eu".into());
    for (k, v) in [
        ("AWS_S3_ENDPOINT", endpoint.as_str()),
        ("AWS_VIRTUAL_HOSTING", "FALSE"), ("AWS_HTTPS", "YES"),
        ("GDAL_DISABLE_READDIR_ON_OPEN", "EMPTY_DIR"),
        ("GDAL_HTTP_MULTIPLEX", "YES"), ("VSI_CACHE", "TRUE"),
    ] {
        let _ = gdal::config::set_config_option(k, v);
    }
}

// s3://… → /vsis3/… ; http(s):// → /vsicurl/… ; /vsi* and local paths pass through.
fn to_vsi(href: &str) -> String {
    if let Some(r) = href.strip_prefix("s3://") { format!("/vsis3/{r}") }
    else if href.starts_with("http://") || href.starts_with("https://") { format!("/vsicurl/{href}") }
    else { href.to_string() }
}

struct Raster {
    ds: Dataset,
    width: usize,
    height: usize,
    res_x: f64,
    res_y: f64,
    bbox: [f64; 4], // min_x, min_y, max_x, max_y (utm)
}

fn open(href: &str) -> Result<Raster, String> {
    let ds = Dataset::open(to_vsi(href)).map_err(|e| format!("open {href}: {e}"))?;
    let gt = ds.geo_transform().map_err(|e| format!("geotransform: {e}"))?;
    let (width, height) = ds.raster_size();
    let (res_x, res_y, min_x, max_y) = (gt[1], -gt[5], gt[0], gt[3]);
    Ok(Raster { ds, width, height, res_x, res_y,
        bbox: [min_x, max_y - height as f64 * res_y, min_x + width as f64 * res_x, max_y] })
}

fn read_window<T: Copy + gdal::raster::GdalType>(r: &Raster, win: [usize; 4]) -> Option<Vec<T>> {
    let [x0, y0, x1, y1] = win;
    let (w, h) = (x1 - x0, y1 - y0);
    if w == 0 || h == 0 { return None; }
    let band = r.ds.rasterband(1).ok()?;
    let buf = band.read_as::<T>((x0 as isize, y0 as isize), (w, h), (w, h), None).ok()?;
    Some(buf.data().to_vec())
}

fn read_overview<T: Copy + gdal::raster::GdalType>(r: &Raster, utm_bbox: [f64; 4], factor: usize) -> Option<Vec<T>> {
    let [min_x, _, _, max_y] = r.bbox;
    let x0 = (((utm_bbox[0] - min_x) / r.res_x).floor().max(0.0)) as usize;
    let y0 = (((max_y - utm_bbox[3]) / r.res_y).floor().max(0.0)) as usize;
    let x1 = (((utm_bbox[2] - min_x) / r.res_x).ceil().min(r.width as f64)) as usize;
    let y1 = (((max_y - utm_bbox[1]) / r.res_y).ceil().min(r.height as f64)) as usize;
    let (w, h) = (x1.checked_sub(x0)?, y1.checked_sub(y0)?);
    if w == 0 || h == 0 { return None; }
    let bw = (w / factor).max(1);
    let bh = (h / factor).max(1);
    let band = r.ds.rasterband(1).ok()?;
    let buf = band.read_as::<T>((x0 as isize, y0 as isize), (w, h), (bw, bh), Some(ResampleAlg::NearestNeighbour)).ok()?;
    Some(buf.data().to_vec())
}

fn harmonize(a: &mut [u16], off: u16) {
    if off == 0 { return; }
    for v in a.iter_mut() { *v = if *v > off { *v - off } else { 0 }; }
}

fn any_hot(a: &[u16], floor: f64) -> bool { a.iter().any(|&v| v as f64 >= floor) }
fn cloud_frac(a: &[u8]) -> f64 {
    if a.is_empty() { return 0.0; }
    a.iter().filter(|&&v| v == 3 || v == 8 || v == 9 || v == 10).count() as f64 / a.len() as f64
}

/// process one scene → (detections, cloud_free). same contract as cog-gdal.js.
pub fn detect_image(item: &Item, bbox: [f64; 4], t: &Thresholds, screen_overview: bool, do_harmonize: bool) -> Result<(Vec<Detection>, bool), String> {
    let off = if do_harmonize { harmonize_offset(&item.date) } else { 0 };
    let b = &item.bands;
    let (b12_url, b11_url) = match (&b.b12, &b.b11) {
        (Some(a), Some(c)) => (a, c),
        _ => return Ok((Vec::new(), true)),
    };

    let b12 = open(b12_url)?;
    let [img_min_x, img_min_y, img_max_x, img_max_y] = b12.bbox;
    let (zone, is_north) = s2_flares_core::utm_params(item.epsg);
    let sw = s2_flares_core::wgs84_to_utm(bbox[0], bbox[1], zone, is_north);
    let ne = s2_flares_core::wgs84_to_utm(bbox[2], bbox[3], zone, is_north);
    let utm_bbox = [sw.0.max(img_min_x), sw.1.max(img_min_y), ne.0.min(img_max_x), ne.1.min(img_max_y)];

    let mut overview_cloud_free = true;
    if screen_overview {
        if let Some(scl_url) = &b.scl {
            if let Ok(scl_r) = open(scl_url) {
                if let Some(ov) = read_overview::<u8>(&scl_r, utm_bbox, OVERVIEW_FACTOR) {
                    let frac = cloud_frac(&ov);
                    if frac > t.max_cloud_local { return Ok((Vec::new(), false)); }
                    overview_cloud_free = frac <= t.cloud_free_thresh;
                }
            }
        }
        if let Some(mut ov) = read_overview::<u16>(&b12, utm_bbox, OVERVIEW_FACTOR) {
            harmonize(&mut ov, off);
            if !any_hot(&ov, b12_dn_min(t)) {
                return Ok((Vec::new(), overview_cloud_free));
            }
        }
    }

    let blocks = enumerate_blocks(b12.width, b12.height, b12.bbox, b12.res_x, b12.res_y, bbox, item.epsg);
    if blocks.is_empty() { return Ok((Vec::new(), overview_cloud_free)); }

    // auxiliary bands opened once, lazily — only if some block has hot b12.
    let mut aux: Option<(Raster, Option<Raster>, Option<Raster>)> = None;
    let mut all = Vec::new();
    let mut all_cloud_free = overview_cloud_free;
    let floor = b12_dn_min(t);

    for blk in &blocks {
        let mut b12_raw = match read_window::<u16>(&b12, blk.window) { Some(v) => v, None => continue };
        harmonize(&mut b12_raw, off);
        if !any_hot(&b12_raw, floor) { continue; }

        if aux.is_none() {
            let b11_r = open(b11_url)?;
            let b8a_r = b.b8a.as_ref().and_then(|u| open(u).ok());
            let scl_r = b.scl.as_ref().and_then(|u| open(u).ok());
            aux = Some((b11_r, b8a_r, scl_r));
        }
        let (b11_r, b8a_r, scl_r) = aux.as_ref().unwrap();

        let mut b11_raw = match read_window::<u16>(b11_r, blk.window) { Some(v) => v, None => continue };
        harmonize(&mut b11_raw, off);
        let b8a_raw = b8a_r.as_ref().and_then(|r| read_window::<u16>(r, blk.window)).map(|mut v| { harmonize(&mut v, off); v });
        let scl_raw = scl_r.as_ref().and_then(|r| read_window::<u8>(r, blk.window));

        let [x0, y0, x1, y1] = blk.window;
        let meta = BlockMeta {
            date: item.date.clone(), epsg: item.epsg,
            img_min_x, img_max_y, res_x: b12.res_x, res_y: b12.res_y,
            block_offset_x: x0, block_offset_y: y0, width: x1 - x0, height: y1 - y0,
            mgrs: item.mgrs.clone(), scene: item.id.clone(),
            sun_elevation: item.sun_elevation, sun_azimuth: item.sun_azimuth,
        };
        let (dets, cloud_free) = detect_block(&b12_raw, &b11_raw, b8a_raw.as_deref(), scl_raw.as_deref(), &meta, t);
        if !cloud_free { all_cloud_free = false; }
        for d in dets {
            // keep only detections whose peak lands in this canonical block (overlap dedup).
            if (d.peak_img_row as usize) / BLOCK_SIZE == blk.br && (d.peak_img_col as usize) / BLOCK_SIZE == blk.bc {
                all.push(d);
            }
        }
    }
    Ok((all, all_cloud_free))
}
