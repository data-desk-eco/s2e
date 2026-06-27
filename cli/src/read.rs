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
use s2_flares_core::{detect_block, enumerate_blocks, Block, BlockMeta, Detection, Thresholds, BLOCK_SIZE, BLOCK_OVERLAP};
use crate::stac::Item;

pub(crate) const OVERVIEW_FACTOR: usize = 8; // 8× = 160 m screen for 20 m bands

/// one hot candidate block — exactly `detect_block`'s inputs plus its canonical
/// (br,bc) for overlap dedup. the reader seam: a `SceneReader` produces these
/// (decode + recall-safe hot prescreen), the shared driver runs `core::detect_block`
/// over them. cpu (gdal windows) and gpu (nvjpeg2000 full-tile) readers differ only
/// in how the pixels arrive; the methodology decision is core's, on both paths.
pub struct Candidate {
    pub meta: BlockMeta,
    pub br: usize,
    pub bc: usize,
    pub b12: Vec<u16>,
    pub b11: Vec<u16>,
    pub b8a: Option<Vec<u16>>,
    pub scl: Option<Vec<u8>>,
}

/// decode + coarse hot-pixel prescreen for one scene → (hot candidates, cloud_free).
/// the ONLY duplicated logic across cpu/gpu: a recall-safe `B12 ≥ hot floor` gate
/// (a strict superset of the core mask) that can never drop a detection core keeps.
pub trait SceneReader: Sync {
    fn candidates(&self, item: &Item, region: [f64; 4], full_tile: bool, t: &Thresholds, screen_overview: bool)
        -> Result<(Vec<Candidate>, bool), String>;
}

/// shared driver — identical for every reader: candidates → `core::detect_block` →
/// detections (kept only where the peak lands in the canonical block: overlap dedup)
/// → cloud_free fold (the reader's overview verdict ∧ every block's). this is where
/// the must-not-drift methodology runs, single-sourced from `core/`.
pub fn detect_candidates(cands: &[Candidate], overview_cloud_free: bool, t: &Thresholds) -> (Vec<Detection>, bool) {
    let mut all = Vec::new();
    let mut cloud_free = overview_cloud_free;
    for c in cands {
        let (dets, cf) = detect_block(&c.b12, &c.b11, c.b8a.as_deref(), c.scl.as_deref(), &c.meta, t);
        if !cf { cloud_free = false; }
        for d in dets {
            if (d.peak_img_row as usize) / BLOCK_SIZE == c.br && (d.peak_img_col as usize) / BLOCK_SIZE == c.bc {
                all.push(d);
            }
        }
    }
    (all, cloud_free)
}

pub(crate) fn b12_dn_min(t: &Thresholds) -> f64 { t.b12_min * 10000.0 + 1000.0 }
pub(crate) fn harmonize_offset(date: &str) -> u16 { if date >= "2022-01-25" { 1000 } else { 0 } }

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
pub(crate) fn to_vsi(href: &str) -> String {
    if let Some(r) = href.strip_prefix("s3://") { format!("/vsis3/{r}") }
    else if href.starts_with("http://") || href.starts_with("https://") { format!("/vsicurl/{href}") }
    else { href.to_string() }
}

pub(crate) struct Raster {
    pub ds: Dataset,
    pub width: usize,
    pub height: usize,
    pub res_x: f64,
    pub res_y: f64,
    pub bbox: [f64; 4], // min_x, min_y, max_x, max_y (utm)
}

pub(crate) fn open(href: &str) -> Result<Raster, String> {
    let ds = Dataset::open(to_vsi(href)).map_err(|e| format!("open {href}: {e}"))?;
    let gt = ds.geo_transform().map_err(|e| format!("geotransform: {e}"))?;
    let (width, height) = ds.raster_size();
    let (res_x, res_y, min_x, max_y) = (gt[1], -gt[5], gt[0], gt[3]);
    Ok(Raster { ds, width, height, res_x, res_y,
        bbox: [min_x, max_y - height as f64 * res_y, min_x + width as f64 * res_x, max_y] })
}

pub(crate) fn read_window<T: Copy + gdal::raster::GdalType>(r: &Raster, win: [usize; 4]) -> Option<Vec<T>> {
    let [x0, y0, x1, y1] = win;
    let (w, h) = (x1 - x0, y1 - y0);
    if w == 0 || h == 0 { return None; }
    let band = r.ds.rasterband(1).ok()?;
    let buf = band.read_as::<T>((x0 as isize, y0 as isize), (w, h), (w, h), None).ok()?;
    Some(buf.data().to_vec())
}

pub(crate) fn read_overview<T: Copy + gdal::raster::GdalType>(r: &Raster, utm_bbox: [f64; 4], factor: usize) -> Option<Vec<T>> {
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

pub(crate) fn harmonize(a: &mut [u16], off: u16) {
    if off == 0 { return; }
    for v in a.iter_mut() { *v = if *v > off { *v - off } else { 0 }; }
}

pub(crate) fn any_hot(a: &[u16], floor: f64) -> bool { a.iter().any(|&v| v as f64 >= floor) }
pub(crate) fn cloud_frac(a: &[u8]) -> f64 {
    if a.is_empty() { return 0.0; }
    a.iter().filter(|&&v| v == 3 || v == 8 || v == 9 || v == 10).count() as f64 / a.len() as f64
}

// --- shared reader pieces (both readers single-source these, so the gpu path can
// never drift from the cpu path on the early-out, aux reads, or block geometry) ---

pub(crate) type Aux = (Raster, Option<Raster>, Option<Raster>); // (b11, b8a, scl)

/// the 8× overview early-out verdict. Skip: too cloudy (→ empty, not cloud-free).
/// Cold: no hot b12 in the overview (→ empty, with the cloud-free flag). Go: proceed.
pub(crate) enum Screen { Skip, Cold(bool), Go(bool) }

/// every block of the whole image — the full-tile work model. mirrors
/// enumerate_blocks' grid/overlap exactly (so the canonical-block dedup still holds),
/// but over the full [0,w]×[0,h] with no wgs84 corner reprojection — UTM is rotated
/// w.r.t. lon/lat, so a 2-corner reproject of the tile's wgs84 bbox clips the corners.
pub(crate) fn all_blocks(w: usize, h: usize) -> Vec<Block> {
    let mut blocks = Vec::new();
    for br in 0..h.div_ceil(BLOCK_SIZE) {
        for bc in 0..w.div_ceil(BLOCK_SIZE) {
            let x0 = (bc * BLOCK_SIZE).saturating_sub(BLOCK_OVERLAP);
            let y0 = (br * BLOCK_SIZE).saturating_sub(BLOCK_OVERLAP);
            let x1 = ((bc + 1) * BLOCK_SIZE + BLOCK_OVERLAP).min(w);
            let y1 = ((br + 1) * BLOCK_SIZE + BLOCK_OVERLAP).min(h);
            blocks.push(Block { br, bc, window: [x0, y0, x1, y1] });
        }
    }
    blocks
}

/// clamp the query bbox to the b12 image extent, in utm — the overview read window.
pub(crate) fn region_utm_bbox(b12: &Raster, bbox: [f64; 4], epsg: i32) -> [f64; 4] {
    let [imx, imy, i_mx, i_my] = b12.bbox;
    let (zone, is_north) = s2_flares_core::utm_params(epsg);
    let sw = s2_flares_core::wgs84_to_utm(bbox[0], bbox[1], zone, is_north);
    let ne = s2_flares_core::wgs84_to_utm(bbox[2], bbox[3], zone, is_north);
    [sw.0.max(imx), sw.1.max(imy), ne.0.min(i_mx), ne.1.min(i_my)]
}

/// the recall-safe overview prescreen — gdal 8× scl + b12 reads, shared verbatim by
/// both readers (the gpu path uses this for the early-out, not a GPU low-res decode,
/// so the decision is byte-identical to the cpu path's).
pub(crate) fn overview_screen(b12: &Raster, scl_url: Option<&str>, utm_bbox: [f64; 4], t: &Thresholds, off: u16) -> Screen {
    let mut cloud_free = true;
    if let Some(u) = scl_url {
        if let Ok(scl_r) = open(u) {
            if let Some(ov) = read_overview::<u8>(&scl_r, utm_bbox, OVERVIEW_FACTOR) {
                let frac = cloud_frac(&ov);
                if frac > t.max_cloud_local { return Screen::Skip; }
                cloud_free = frac <= t.cloud_free_thresh;
            }
        }
    }
    if let Some(mut ov) = read_overview::<u16>(b12, utm_bbox, OVERVIEW_FACTOR) {
        harmonize(&mut ov, off);
        if !any_hot(&ov, b12_dn_min(t)) { return Screen::Cold(cloud_free); }
    }
    Screen::Go(cloud_free)
}

/// open the aux bands once (b11 required; b8a/scl best-effort) — sparse, lazy: only
/// after the first hot b12 block, on both paths.
pub(crate) fn aux_open(b: &crate::stac::Bands, b11_url: &str) -> Result<Aux, String> {
    Ok((open(b11_url)?, b.b8a.as_ref().and_then(|u| open(u).ok()), b.scl.as_ref().and_then(|u| open(u).ok())))
}

/// assemble one candidate from an already-decoded+harmonised b12 block and the aux
/// rasters (aux windows read + harmonised here). `geom` = (img_min_x, img_max_y,
/// res_x, res_y). identical for both readers — only b12's *source* differs.
pub(crate) fn make_candidate(blk: &Block, b12: Vec<u16>, aux: &Aux, item: &Item, geom: (f64, f64, f64, f64), off: u16) -> Option<Candidate> {
    let (b11_r, b8a_r, scl_r) = aux;
    let mut b11 = read_window::<u16>(b11_r, blk.window)?;
    harmonize(&mut b11, off);
    let b8a = b8a_r.as_ref().and_then(|r| read_window::<u16>(r, blk.window)).map(|mut v| { harmonize(&mut v, off); v });
    let scl = scl_r.as_ref().and_then(|r| read_window::<u8>(r, blk.window));
    let [x0, y0, x1, y1] = blk.window;
    let (img_min_x, img_max_y, res_x, res_y) = geom;
    let meta = BlockMeta {
        date: item.date.clone(), epsg: item.epsg, img_min_x, img_max_y, res_x, res_y,
        block_offset_x: x0, block_offset_y: y0, width: x1 - x0, height: y1 - y0,
        mgrs: item.mgrs.clone(), scene: item.id.clone(),
        sun_elevation: item.sun_elevation, sun_azimuth: item.sun_azimuth,
    };
    Some(Candidate { meta, br: blk.br, bc: blk.bc, b12, b11, b8a, scl })
}

/// the gdal windowed reader — today's logic, now a `SceneReader` over the shared
/// pieces: 8× overview prescreen + per-block windowed reads + lazy aux bands.
/// `harmonize` is the source profile (eodata jp2 carries the N0400 offset; aws cogs
/// don't). the cpu site path; also the gpu path's aux-band reader (sparse windows).
pub struct GdalReader {
    pub harmonize: bool,
}

impl SceneReader for GdalReader {
    fn candidates(&self, item: &Item, bbox: [f64; 4], full_tile: bool, t: &Thresholds, screen_overview: bool)
        -> Result<(Vec<Candidate>, bool), String> {
        let off = if self.harmonize { harmonize_offset(&item.date) } else { 0 };
        let b = &item.bands;
        let (b12_url, b11_url) = match (&b.b12, &b.b11) {
            (Some(a), Some(c)) => (a, c),
            _ => return Ok((Vec::new(), true)),
        };

        let b12 = open(b12_url)?;
        let geom = (b12.bbox[0], b12.bbox[3], b12.res_x, b12.res_y);
        // full_tile screens/enumerates the whole image; otherwise the clamped query window.
        let utm_bbox = if full_tile { b12.bbox } else { region_utm_bbox(&b12, bbox, item.epsg) };

        let overview_cloud_free = match if screen_overview {
            overview_screen(&b12, b.scl.as_deref(), utm_bbox, t, off)
        } else { Screen::Go(true) } {
            Screen::Skip => return Ok((Vec::new(), false)),
            Screen::Cold(cf) => return Ok((Vec::new(), cf)),
            Screen::Go(cf) => cf,
        };

        let blocks = if full_tile { all_blocks(b12.width, b12.height) }
            else { enumerate_blocks(b12.width, b12.height, b12.bbox, b12.res_x, b12.res_y, bbox, item.epsg) };
        let mut aux: Option<Aux> = None;
        let mut cands = Vec::new();
        let floor = b12_dn_min(t);

        for blk in &blocks {
            let mut b12_raw = match read_window::<u16>(&b12, blk.window) { Some(v) => v, None => continue };
            harmonize(&mut b12_raw, off);
            if !any_hot(&b12_raw, floor) { continue; }
            if aux.is_none() { aux = Some(aux_open(b, b11_url)?); }
            if let Some(c) = make_candidate(blk, b12_raw, aux.as_ref().unwrap(), item, geom, off) { cands.push(c); }
        }
        Ok((cands, overview_cloud_free))
    }
}

/// pick the reader: `--gpu` → nvJPEG2000 full-tile (only in a `--features gpu` build),
/// else the gdal windowed reader. boxed behind the trait so the driver is reader-agnostic.
pub fn make_reader(gpu: bool, harmonize: bool) -> Result<Box<dyn SceneReader>, String> {
    if gpu {
        #[cfg(feature = "gpu")]
        { return Ok(Box::new(crate::gpu::GpuReader::new(harmonize))); }
        #[cfg(not(feature = "gpu"))]
        { return Err("--gpu needs a build with --features gpu (CUDA box)".into()); }
    }
    Ok(Box::new(GdalReader { harmonize }))
}

/// the reader seam composed: decode + prescreen (reader) → `core::detect_block` (driver).
pub fn detect_scene(r: &dyn SceneReader, item: &Item, bbox: [f64; 4], full_tile: bool, t: &Thresholds, screen_overview: bool) -> Result<(Vec<Detection>, bool), String> {
    let (cands, ocf) = r.candidates(item, bbox, full_tile, t, screen_overview)?;
    Ok(detect_candidates(&cands, ocf, t))
}
