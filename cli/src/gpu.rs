//! the gpu full-tile reader — nvJPEG2000 decode of the WHOLE B12 tile, then the
//! exact same shared pieces as `GdalReader` (overview early-out, aux windows,
//! candidate assembly). only b12's *source* differs: one GPU tile decode replaces
//! N per-block OpenJPEG reads. JP2 is lossless → identical integer pixels, so the
//! detections match the cpu path byte-for-byte (the parity test below is the gate).
//! aux bands (b11/b8a/scl) stay on the gdal windowed path — sparse, so cheap.

use s2_flares_core::{enumerate_blocks, Thresholds};
use crate::read::{self, Candidate, SceneReader};
use crate::stac::Item;

pub struct GpuReader { harmonize: bool }
impl GpuReader { pub fn new(harmonize: bool) -> Self { Self { harmonize } } }

// row-major slice of a full tile into a block window [x0,y0,x1,y1] — the exact pixels
// gdal's read_window would return, so the candidate's b12 is byte-identical.
fn slice(full: &[u16], tile_w: usize, win: [usize; 4]) -> Vec<u16> {
    let [x0, y0, x1, y1] = win;
    let mut out = Vec::with_capacity((x1 - x0) * (y1 - y0));
    for row in y0..y1 { let base = row * tile_w; out.extend_from_slice(&full[base + x0..base + x1]); }
    out
}

// read a whole /vsis3 object into memory — nvjpeg2000 needs the raw JP2 codestream;
// gdal's VSI layer fetches it with the same per-VM eodata creds gdal already uses.
fn vsi_bytes(href: &str) -> Result<Vec<u8>, String> {
    use gdal_sys::{VSIFCloseL, VSIFOpenL, VSIFReadL, VSIFSeekL, VSIFTellL};
    let path = std::ffi::CString::new(read::to_vsi(href)).map_err(|e| e.to_string())?;
    let mode = std::ffi::CString::new("rb").unwrap();
    unsafe {
        let f = VSIFOpenL(path.as_ptr(), mode.as_ptr());
        if f.is_null() { return Err(format!("vsi open {href}")); }
        VSIFSeekL(f, 0, 2 /*SEEK_END*/);
        let len = VSIFTellL(f) as usize;
        VSIFSeekL(f, 0, 0 /*SEEK_SET*/);
        let mut buf = vec![0u8; len];
        let n = VSIFReadL(buf.as_mut_ptr() as *mut std::ffi::c_void, 1, len, f);
        VSIFCloseL(f);
        if n != len { return Err(format!("vsi short read {href}: {n}/{len}")); }
        Ok(buf)
    }
}

impl SceneReader for GpuReader {
    fn candidates(&self, item: &Item, bbox: [f64; 4], full_tile: bool, t: &Thresholds, screen_overview: bool)
        -> Result<(Vec<Candidate>, bool), String> {
        let off = if self.harmonize { read::harmonize_offset(&item.date) } else { 0 };
        let b = &item.bands;
        let (b12_url, b11_url) = match (&b.b12, &b.b11) {
            (Some(a), Some(c)) => (a, c),
            _ => return Ok((Vec::new(), true)),
        };

        // cheap gdal open for geometry/dims (decode is the cost, not open).
        let b12 = read::open(b12_url)?;
        let geom = (b12.bbox[0], b12.bbox[3], b12.res_x, b12.res_y);
        let utm_bbox = if full_tile { b12.bbox } else { read::region_utm_bbox(&b12, bbox, item.epsg) };

        // shared overview early-out (gdal 8×) — byte-identical decision to the cpu path.
        let ocf = match if screen_overview {
            read::overview_screen(&b12, b.scl.as_deref(), utm_bbox, t, off)
        } else { read::Screen::Go(true) } {
            read::Screen::Skip => return Ok((Vec::new(), false)),
            read::Screen::Cold(cf) => return Ok((Vec::new(), cf)),
            read::Screen::Go(cf) => cf,
        };

        // the GPU step: one nvjpeg2000 decode of the whole B12 tile, harmonised once.
        let (mut full, w, h) = s2_flares_gpu::decode_b12(&vsi_bytes(b12_url)?)?;
        if w != b12.width || h != b12.height {
            return Err(format!("gpu/gdal dim mismatch {w}x{h} vs {}x{}", b12.width, b12.height));
        }
        read::harmonize(&mut full, off);

        // identical block enumeration + per-block hot prescreen + lazy aux as the cpu
        // path; only b12 comes from the decoded tile (slice) not a windowed gdal read.
        let blocks = if full_tile { read::all_blocks(b12.width, b12.height) }
            else { enumerate_blocks(b12.width, b12.height, b12.bbox, b12.res_x, b12.res_y, bbox, item.epsg) };
        let mut aux: Option<read::Aux> = None;
        let mut cands = Vec::new();
        let floor = read::b12_dn_min(t);
        for blk in &blocks {
            let b12_raw = slice(&full, b12.width, blk.window);
            if !read::any_hot(&b12_raw, floor) { continue; }
            if aux.is_none() { aux = Some(read::aux_open(b, b11_url)?); }
            if let Some(c) = read::make_candidate(blk, b12_raw, aux.as_ref().unwrap(), item, geom, off) { cands.push(c); }
        }
        Ok((cands, ocf))
    }
}

// the frozen-methodology gate extended to the GPU path: GPU detections == CPU
// detections, byte-for-byte, over real scenes. needs a CUDA box + eodata creds + net,
// so #[ignore]'d — run it on the box (box.sh parity does exactly this):
//   S2_PARITY_BBOX=W,S,E,N [S2_PARITY_TILE=39RWN] [S2_PARITY_START=… S2_PARITY_END=…] \
//     cargo test -p s2-flares-cli --release --features gpu parity -- --ignored --nocapture
#[cfg(test)]
mod parity {
    use super::*;
    use crate::read::{detect_scene, GdalReader};

    #[test]
    #[ignore]
    fn gpu_matches_cpu() {
        read::configure();
        let bbox = env_bbox();
        let (start, end) = (env_or("S2_PARITY_START", "2024-01-01"), env_or("S2_PARITY_END", "2024-12-31"));
        let mut items = crate::stac::search(bbox, &start, &end, 100.0, "cdse").expect("stac search");
        if let Ok(tile) = std::env::var("S2_PARITY_TILE") { items.retain(|i| i.mgrs == tile); }
        items.truncate(3);
        assert!(!items.is_empty(), "no scenes for the parity bbox/date window");
        let (cpu, gpu, t) = (GdalReader { harmonize: true }, GpuReader::new(true), Thresholds::default());
        for it in &items {
            let c = detect_scene(&cpu, it, bbox, false, &t, false).expect("cpu detect").0;
            let g = detect_scene(&gpu, it, bbox, false, &t, false).expect("gpu detect").0;
            assert_eq!(c.len(), g.len(), "det count mismatch {} {}", it.mgrs, it.date);
            for (a, b) in c.iter().zip(g.iter()) {
                assert_eq!((a.lon.to_bits(), a.lat.to_bits(), a.max_b12.to_bits(), a.avg_b12.to_bits(), a.pixels),
                           (b.lon.to_bits(), b.lat.to_bits(), b.max_b12.to_bits(), b.avg_b12.to_bits(), b.pixels),
                           "detection mismatch {} {}", it.mgrs, it.date);
            }
            eprintln!("parity OK {} {} — {} detections", it.mgrs, it.date, c.len());
        }
    }

    fn env_or(k: &str, d: &str) -> String { std::env::var(k).unwrap_or_else(|_| d.into()) }
    fn env_bbox() -> [f64; 4] {
        let s = std::env::var("S2_PARITY_BBOX").expect("set S2_PARITY_BBOX=W,S,E,N");
        let v: Vec<f64> = s.split(',').map(|x| x.trim().parse().unwrap()).collect();
        [v[0], v[1], v[2], v[3]]
    }
}
