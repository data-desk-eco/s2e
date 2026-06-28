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
use s2_flares_core::{detect_block, enumerate_blocks, cover_sites, grid_sites, Block, BlockMeta, Detection, Site, Thresholds, BLOCK_SIZE, BLOCK_OVERLAP};
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
        // retry transient s3 errors instead of returning a NULL dataset on the first
        // hiccup — fleet-wide concurrency on /vsis3 eodata throttles, and an un-retried
        // GDALOpenEx NULL was failing whole scenes. (also covers the aws /vsicurl path.)
        ("GDAL_HTTP_MAX_RETRY", "5"), ("GDAL_HTTP_RETRY_DELAY", "1"),
    ] {
        let _ = gdal::config::set_config_option(k, v);
    }
}

// s3://… → /vsis3/… ; http(s):// → /vsicurl/… ; /vsi* and local paths pass through.
// (the /eodata s3fs mount was tried and is unreliable for this many-open access pattern
// — 0/12 vs /vsis3's 12/12 sequentially — so eodata stays on /vsis3, hardened by the
// GDAL HTTP retries above + the open() retry below.)
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
    // retry the open with backoff — a transient eodata throttle returns a NULL dataset
    // (GDAL's own GDAL_HTTP_MAX_RETRY doesn't catch every curl-level failure), and a
    // single hiccup must not fail a whole scene. ~0.4/0.8/1.6s between four attempts.
    let path = to_vsi(href);
    let mut ds = Dataset::open(&path);
    for attempt in 1..4 {
        if ds.is_ok() { break; }
        std::thread::sleep(std::time::Duration::from_millis(200u64 << attempt));
        ds = Dataset::open(&path);
    }
    let ds = ds.map_err(|e| format!("open {href}: {e}"))?;
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

/// the per-block detect.js metadata (geo-anchor + sun geometry). `geom` = (img_min_x,
/// img_max_y, res_x, res_y). shared by both readers — the candidate's pixel *source*
/// differs (windowed gdal vs sliced-from-resident), this stays identical.
pub(crate) fn block_meta(blk: &Block, item: &Item, geom: (f64, f64, f64, f64)) -> BlockMeta {
    let [x0, y0, x1, y1] = blk.window;
    let (img_min_x, img_max_y, res_x, res_y) = geom;
    BlockMeta {
        date: item.date.clone(), epsg: item.epsg, img_min_x, img_max_y, res_x, res_y,
        block_offset_x: x0, block_offset_y: y0, width: x1 - x0, height: y1 - y0,
        mgrs: item.mgrs.clone(), scene: item.id.clone(),
        sun_elevation: item.sun_elevation, sun_azimuth: item.sun_azimuth,
    }
}

/// assemble one candidate from an already-decoded+harmonised b12 block and the aux
/// rasters (aux windows read + harmonised here). identical for both readers — only
/// b12's *source* differs. the windowed `GdalReader`'s lazy-aux assembler.
pub(crate) fn make_candidate(blk: &Block, b12: Vec<u16>, aux: &Aux, item: &Item, geom: (f64, f64, f64, f64), off: u16) -> Option<Candidate> {
    let (b11_r, b8a_r, scl_r) = aux;
    let mut b11 = read_window::<u16>(b11_r, blk.window)?;
    harmonize(&mut b11, off);
    let b8a = b8a_r.as_ref().and_then(|r| read_window::<u16>(r, blk.window)).map(|mut v| { harmonize(&mut v, off); v });
    let scl = scl_r.as_ref().and_then(|r| read_window::<u8>(r, blk.window));
    Some(Candidate { meta: block_meta(blk, item, geom), br: blk.br, bc: blk.bc, b12, b11, b8a, scl })
}

/// row-major slice of a resident full tile into a block window [x0,y0,x1,y1] — the
/// exact pixels a windowed gdal `read_window` returns, so candidates are byte-identical.
fn slice<T: Copy>(full: &[T], tile_w: usize, win: [usize; 4]) -> Vec<T> {
    let [x0, y0, x1, y1] = win;
    let mut out = Vec::with_capacity((x1 - x0) * (y1 - y0));
    for row in y0..y1 { let base = row * tile_w; out.extend_from_slice(&full[base + x0..base + x1]); }
    out
}

/// one gdal whole-band RasterIO (the bulk reader's cpu decode: replaces hundreds of
/// windowed decodes with a single full-tile read).
fn whole<T: Copy + gdal::raster::GdalType>(r: &Raster) -> Option<Vec<T>> {
    read_window::<T>(r, [0, 0, r.width, r.height])
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

/// the bulk full-tile reader: for the `--region` wide-area path, fetch +
/// decode each band WHOLE once, hold all bands resident, then iterate `all_blocks`
/// slicing from RAM — **zero per-block I/O**. windowed range-reads re-do codestream
/// decode per block (the ~12s/tile that left the GPU idle); one whole-tile decode
/// replaces them. cpu: one gdal whole-band RasterIO per band. gpu: nvjpeg2000 batched
/// over the spectral bands (decode becomes the dominant step — the step the GPU wins).
/// the sparse point-AOI default stays the windowed `GdalReader`.
pub struct BulkReader { pub gpu: bool, pub harmonize: bool }

impl SceneReader for BulkReader {
    fn candidates(&self, item: &Item, bbox: [f64; 4], full_tile: bool, t: &Thresholds, screen_overview: bool)
        -> Result<(Vec<Candidate>, bool), String> {
        let off = if self.harmonize { harmonize_offset(&item.date) } else { 0 };
        let b = &item.bands;
        let (b12_url, b11_url) = match (&b.b12, &b.b11) {
            (Some(a), Some(c)) => (a, c),
            _ => return Ok((Vec::new(), true)),
        };

        let b12 = open(b12_url)?;
        let (w, h) = (b12.width, b12.height);
        let geom = (b12.bbox[0], b12.bbox[3], b12.res_x, b12.res_y);
        let utm_bbox = if full_tile { b12.bbox } else { region_utm_bbox(&b12, bbox, item.epsg) };

        let ocf = match if screen_overview {
            overview_screen(&b12, b.scl.as_deref(), utm_bbox, t, off)
        } else { Screen::Go(true) } {
            Screen::Skip => return Ok((Vec::new(), false)),
            Screen::Cold(cf) => return Ok((Vec::new(), cf)),
            Screen::Go(cf) => cf,
        };

        // decode every band WHOLE — the bulk move. b12/b11 required; b8a/scl best-effort.
        let (mut full_b12, mut full_b11, full_b8a, full_scl);
        if self.gpu {
            #[cfg(feature = "gpu")] {
                let (f12, f11, f8a) = crate::gpu::decode_bands(b12_url, b11_url, b.b8a.as_deref())?;
                if f12.len() != w * h || f11.len() != w * h { return Err("gpu/gdal dim mismatch".into()); }
                full_b12 = f12; full_b11 = f11; full_b8a = f8a;
                full_scl = b.scl.as_deref().and_then(|u| open(u).ok()).and_then(|r| whole::<u8>(&r));
            }
            #[cfg(not(feature = "gpu"))]
            { return Err("--gpu needs a --features gpu build (CUDA box)".into()); }
        } else {
            full_b12 = whole::<u16>(&b12).ok_or("b12 whole-band read")?;
            full_b11 = whole::<u16>(&open(b11_url)?).ok_or("b11 whole-band read")?;
            full_b8a = b.b8a.as_deref().and_then(|u| open(u).ok()).and_then(|r| whole::<u16>(&r));
            full_scl = b.scl.as_deref().and_then(|u| open(u).ok()).and_then(|r| whole::<u8>(&r));
        }
        harmonize(&mut full_b12, off);
        harmonize(&mut full_b11, off);
        let full_b8a = full_b8a.map(|mut v| { harmonize(&mut v, off); v });

        // iterate blocks, slice each band from the resident tiles, prescreen, assemble.
        let blocks = if full_tile { all_blocks(w, h) }
            else { enumerate_blocks(w, h, b12.bbox, b12.res_x, b12.res_y, bbox, item.epsg) };
        let floor = b12_dn_min(t);
        let mut cands = Vec::new();
        for blk in &blocks {
            let b12s = slice(&full_b12, w, blk.window);
            if !any_hot(&b12s, floor) { continue; }
            cands.push(Candidate {
                meta: block_meta(blk, item, geom), br: blk.br, bc: blk.bc,
                b12: b12s, b11: slice(&full_b11, w, blk.window),
                b8a: full_b8a.as_ref().map(|f| slice(f, w, blk.window)),
                scl: full_scl.as_ref().map(|f| slice(f, w, blk.window)),
            });
        }
        Ok((cands, ocf))
    }
}

/// pick the reader: `--region` → the bulk full-tile reader (whole-band decode, in-RAM
/// detect), `--gpu` adding nvJPEG2000 batched decode (only in a `--features gpu` build);
/// else the gdal windowed reader (sparse point AOIs). boxed behind the trait so the
/// driver is reader-agnostic.
pub fn make_reader(gpu: bool, bulk: bool, harmonize: bool) -> Result<Box<dyn SceneReader>, String> {
    if gpu && !cfg!(feature = "gpu") { return Err("--gpu needs a --features gpu build (CUDA box)".into()); }
    if gpu && !bulk { return Err("--gpu is the bulk full-tile path — use it with --region".into()); }
    Ok(if bulk { Box::new(BulkReader { gpu, harmonize }) } else { Box::new(GdalReader { harmonize }) })
}

/// sample the SCL band at each site for one scene → (site_id, cloud_frac) for the
/// in-footprint sites only. the I/O half of the site-anchored clear-sky denominator;
/// the windowing/classification is `core::cover_sites`. SCL carries no harmonisation
/// offset (it's a class band), so no per-date shift. one whole-band SCL read per scene
/// (a single 20 m band) — cheap in-region, the second pass `cover_sites` was built for.
pub fn cover_scene(item: &Item, sites: &[Site]) -> Vec<(String, f64)> {
    let url = match &item.bands.scl { Some(u) => u, None => return Vec::new() };
    let r = match open(url) { Ok(r) => r, Err(_) => return Vec::new() };
    let scl = match whole::<u8>(&r) { Some(v) => v, None => return Vec::new() };
    cover_sites(&scl, r.width, r.height, r.bbox, item.epsg, sites).into_iter()
        .map(|c| { let f = c.cloud_frac(); (c.h3, f) }).collect()
}

/// the cloud-mask slice for one scene (the persistence fold-in): one whole-band SCL
/// read, sampled over a ~100 m grid covering the scan window → (cell_key, cloud_frac)
/// per observed cell. emitted for EVERY scene incl. flareless/cloudy — those clear-but-
/// unlit looks are the honest persistence denominator the detection archive can't carry.
/// classifier shared with the cluster join via `core` (CoverRow::cloud_frac), so the
/// detection-time mask and the cluster-time denominator can't drift.
///
/// CRITICAL: the grid must cover the SAME footprint the detector scans, not the raw AOI
/// bbox. `enumerate_blocks` rounds the AOI out to ~512 px blocks (+overlap) and
/// `detect_block` reports hits across the WHOLE block — so detections (and thus clusters)
/// spread up to ~10 km beyond the AOI polygon. gridding only the AOI bbox left those edge
/// clusters with no cell to join. so we grid the union of the detector's blocks (same
/// `enumerate_blocks`/`all_blocks` call), reprojected pixel→wgs84.
pub fn cloud_scene(item: &Item, bbox: [f64; 4], full_tile: bool) -> Vec<(String, f64)> {
    let url = match &item.bands.scl { Some(u) => u, None => return Vec::new() };
    let r = match open(url) { Ok(r) => r, Err(_) => return Vec::new() };
    let scl = match whole::<u8>(&r) { Some(v) => v, None => return Vec::new() };
    // the detector's exact block footprint (SCL is 20 m, same grid/extent as B12).
    let blocks = if full_tile { all_blocks(r.width, r.height) }
        else { enumerate_blocks(r.width, r.height, r.bbox, r.res_x, r.res_y, bbox, item.epsg) };
    if blocks.is_empty() { return Vec::new(); }
    let (mut x0, mut y0, mut x1, mut y1) = (usize::MAX, usize::MAX, 0usize, 0usize);
    for b in &blocks { x0 = x0.min(b.window[0]); y0 = y0.min(b.window[1]); x1 = x1.max(b.window[2]); y1 = y1.max(b.window[3]); }
    // pixel union → utm → wgs84 bbox (utm is rotated, so reproject all four corners).
    let (zone, isn) = s2_flares_core::utm_params(item.epsg);
    let px = |x: usize, y: usize| s2_flares_core::utm_to_wgs84(r.bbox[0] + x as f64 * r.res_x, r.bbox[3] - y as f64 * r.res_y, zone, isn);
    let c = [px(x0, y0), px(x1, y0), px(x0, y1), px(x1, y1)];
    let wb = [c.iter().map(|p| p.0).fold(f64::INFINITY, f64::min), c.iter().map(|p| p.1).fold(f64::INFINITY, f64::min),
              c.iter().map(|p| p.0).fold(f64::NEG_INFINITY, f64::max), c.iter().map(|p| p.1).fold(f64::NEG_INFINITY, f64::max)];
    cover_sites(&scl, r.width, r.height, r.bbox, item.epsg, &grid_sites(wb)).into_iter()
        .map(|c| { let f = c.cloud_frac(); (c.h3, f) }).collect()
}

/// the reader seam composed: decode + prescreen (reader) → `core::detect_block` (driver).
pub fn detect_scene(r: &dyn SceneReader, item: &Item, bbox: [f64; 4], full_tile: bool, t: &Thresholds, screen_overview: bool) -> Result<(Vec<Detection>, bool), String> {
    let (cands, ocf) = r.candidates(item, bbox, full_tile, t, screen_overview)?;
    Ok(detect_candidates(&cands, ocf, t))
}

// cpu-only half of the parity gate: bulk whole-band slicing == windowed reads, over
// real (anonymous AWS COG) scenes — runs anywhere with net, no CUDA/eodata. the gpu
// half lives in gpu.rs (box-only). #[ignore]'d (network):
//   S2_PARITY_BBOX=W,S,E,N cargo test -p s2-flares-cli --release parity_cpu -- --ignored --nocapture
#[cfg(test)]
mod parity_cpu {
    use super::*;
    #[test]
    #[ignore]
    fn bulk_matches_windowed() {
        configure();
        let s = std::env::var("S2_PARITY_BBOX").expect("set S2_PARITY_BBOX=W,S,E,N");
        let v: Vec<f64> = s.split(',').map(|x| x.trim().parse().unwrap()).collect();
        let bbox = [v[0], v[1], v[2], v[3]];
        let (start, end) = (std::env::var("S2_PARITY_START").unwrap_or_else(|_| "2024-01-01".into()),
                            std::env::var("S2_PARITY_END").unwrap_or_else(|_| "2024-12-31".into()));
        let mut items = crate::stac::search(bbox, &start, &end, 100.0, "aws").expect("stac search");
        items.truncate(3);
        assert!(!items.is_empty(), "no scenes for the parity bbox/window");
        let (t, win, bulk) = (Thresholds::default(), GdalReader { harmonize: false }, BulkReader { gpu: false, harmonize: false });
        for it in &items {
            let a = detect_scene(&win, it, bbox, false, &t, false).expect("windowed").0;
            let b = detect_scene(&bulk, it, bbox, false, &t, false).expect("bulk").0;
            assert_eq!(a.len(), b.len(), "count mismatch {} {}", it.mgrs, it.date);
            for (x, y) in a.iter().zip(b.iter()) {
                assert_eq!((x.lon.to_bits(), x.lat.to_bits(), x.max_b12.to_bits(), x.pixels),
                           (y.lon.to_bits(), y.lat.to_bits(), y.max_b12.to_bits(), y.pixels),
                           "mismatch {} {}", it.mgrs, it.date);
            }
            eprintln!("cpu parity OK {} {} — {} detections", it.mgrs, it.date, a.len());
        }
    }
}
