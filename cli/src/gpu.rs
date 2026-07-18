//! the gpu side of the bulk reader: fetch a scene's spectral band codestreams whole
//! off /vsis3 and decode them together with nvJPEG2000 (batched, one cuda stream each).
//! lossless JP2 → identical integer pixels to gdal, so `BulkReader{gpu}` detections
//! match the windowed cpu path byte-for-byte (the parity test below is the gate). only
//! the 16-bit spectral bands go to the GPU; scl (8-bit) stays on gdal in the reader.

use crate::read;

// read a whole /vsis3 object into memory — nvjpeg2000 needs the raw JP2 codestream;
// gdal's VSI layer fetches it with the same per-VM eodata creds gdal already uses.
fn vsi_bytes(href: &str) -> Result<Vec<u8>, String> {
    use gdal_sys::{VSIFCloseL, VSIFOpenL, VSIFReadL, VSIFSeekL, VSIFTellL};
    let path = std::ffi::CString::new(read::to_vsi(href)).map_err(|e| e.to_string())?;
    let mode = std::ffi::CString::new("rb").unwrap();
    unsafe {
        let f = VSIFOpenL(path.as_ptr(), mode.as_ptr());
        if f.is_null() {
            return Err(format!("vsi open {href}"));
        }
        VSIFSeekL(f, 0, 2 /*SEEK_END*/);
        let len = VSIFTellL(f) as usize;
        VSIFSeekL(f, 0, 0 /*SEEK_SET*/);
        let mut buf = vec![0u8; len];
        let n = VSIFReadL(buf.as_mut_ptr() as *mut std::ffi::c_void, 1, len, f);
        VSIFCloseL(f);
        if n != len {
            return Err(format!("vsi short read {href}: {n}/{len}"));
        }
        Ok(buf)
    }
}

/// fetch + GPU-decode a scene's 16-bit spectral bands (b12, b11 required; b8a if
/// present) → resident whole-tile buffers, in that order. one sequential GET per band
/// (~0.12s, measured) then one decode call (the dominant, GPU-friendly step that, with
/// scene concurrency, runs on the idle GPU while the CPUs crunch the frozen detect).
pub fn decode_bands(
    b12_url: &str,
    b11_url: &str,
    b8a_url: Option<&str>,
) -> Result<(Vec<u16>, Vec<u16>, Option<Vec<u16>>), String> {
    let b12 = vsi_bytes(b12_url)?;
    let b11 = vsi_bytes(b11_url)?;
    let b8a = b8a_url.map(vsi_bytes).transpose()?;
    let mut streams: Vec<&[u8]> = vec![&b12, &b11];
    if let Some(ref a) = b8a {
        streams.push(a);
    }
    let mut out = s2e_gpu::decode_batch(&streams)?.into_iter();
    let f12 = out.next().ok_or("gpu decode: missing b12")?.0;
    let f11 = out.next().ok_or("gpu decode: missing b11")?.0;
    let f8a = b8a.is_some().then(|| out.next().map(|t| t.0)).flatten();
    Ok((f12, f11, f8a))
}

// the frozen-methodology gate: over real scenes the bulk readers (cpu whole-band and
// gpu nvJPEG2000) produce the SAME detections as the windowed gdal reader, byte-for-
// byte — JP2 is lossless so whole-tile decode yields the same pixels as windowed. needs
// a CUDA box + eodata creds + net, so #[ignore]'d — run it on the box (box.sh parity):
//   S2_PARITY_BBOX=W,S,E,N [S2_PARITY_TILE=39RWN] [S2_PARITY_START=… S2_PARITY_END=…] \
//     cargo test -p s2e-cli --release --features gpu parity -- --ignored --nocapture
#[cfg(test)]
mod parity {
    use super::*;
    use crate::read::{detect_scene, BulkReader, GdalReader};
    use s2e_core::{Detection, Thresholds};

    #[test]
    #[ignore]
    fn bulk_matches_windowed() {
        read::configure();
        let bbox = env_bbox();
        let (start, end) = (
            env_or("S2_PARITY_START", "2024-01-01"),
            env_or("S2_PARITY_END", "2024-12-31"),
        );
        let mut items =
            crate::stac::search(bbox, &start, &end, 100.0, "cdse").expect("stac search");
        if let Ok(tile) = std::env::var("S2_PARITY_TILE") {
            items.retain(|i| i.mgrs == tile);
        }
        items.truncate(3);
        assert!(
            !items.is_empty(),
            "no scenes for the parity bbox/date window"
        );
        let t = Thresholds::default();
        let win = GdalReader;
        let bulk_cpu = BulkReader { gpu: false };
        let bulk_gpu = BulkReader { gpu: true };
        for it in &items {
            let w = detect_scene(&win, it, bbox, false, &t, false)
                .expect("windowed detect")
                .0;
            let c = detect_scene(&bulk_cpu, it, bbox, false, &t, false)
                .expect("bulk-cpu detect")
                .0;
            let g = detect_scene(&bulk_gpu, it, bbox, false, &t, false)
                .expect("bulk-gpu detect")
                .0;
            same(&w, &c, "bulk-cpu", it);
            same(&w, &g, "bulk-gpu", it);
            eprintln!("parity OK {} {} — {} detections", it.mgrs, it.date, w.len());
        }
    }

    fn same(a: &[Detection], b: &[Detection], who: &str, it: &crate::stac::Item) {
        assert_eq!(
            a.len(),
            b.len(),
            "{who} det count mismatch {} {}",
            it.mgrs,
            it.date
        );
        for (x, y) in a.iter().zip(b.iter()) {
            assert_eq!(
                (
                    x.lon.to_bits(),
                    x.lat.to_bits(),
                    x.max_b12.to_bits(),
                    x.avg_b12.to_bits(),
                    x.pixels
                ),
                (
                    y.lon.to_bits(),
                    y.lat.to_bits(),
                    y.max_b12.to_bits(),
                    y.avg_b12.to_bits(),
                    y.pixels
                ),
                "{who} detection mismatch {} {}",
                it.mgrs,
                it.date
            );
        }
    }

    fn env_or(k: &str, d: &str) -> String {
        std::env::var(k).unwrap_or_else(|_| d.into())
    }
    fn env_bbox() -> [f64; 4] {
        let s = std::env::var("S2_PARITY_BBOX").expect("set S2_PARITY_BBOX=W,S,E,N");
        let v: Vec<f64> = s.split(',').map(|x| x.trim().parse().unwrap()).collect();
        [v[0], v[1], v[2], v[3]]
    }
}

// full-tile throughput bench: time the windowed reader (the previous impl) vs the
// bulk readers (cpu whole-band, gpu nvJPEG2000) over real whole tiles. box-only.
//   S2_BENCH_BBOX=W,S,E,N [S2_BENCH_TILE=39RWN] [S2_BENCH_N=3] [S2_BENCH_START=… END=…] \
//     cargo test -p s2e-cli --release --features gpu bench_readers -- --ignored --nocapture
#[cfg(test)]
mod bench {
    use crate::read::{detect_scene, BulkReader, GdalReader, SceneReader};
    use s2e_core::Thresholds;
    use std::time::Instant;

    #[test]
    #[ignore]
    fn bench_readers() {
        crate::read::configure();
        let s = std::env::var("S2_BENCH_BBOX").expect("set S2_BENCH_BBOX=W,S,E,N");
        let v: Vec<f64> = s.split(',').map(|x| x.trim().parse().unwrap()).collect();
        let bbox = [v[0], v[1], v[2], v[3]];
        let go = |k: &str, d: &str| std::env::var(k).unwrap_or_else(|_| d.into());
        let (start, end) = (
            go("S2_BENCH_START", "2024-08-01"),
            go("S2_BENCH_END", "2024-08-31"),
        );
        let n: usize = go("S2_BENCH_N", "3").parse().unwrap();
        let mut items =
            crate::stac::search(bbox, &start, &end, 100.0, "cdse").expect("stac search");
        if let Ok(tile) = std::env::var("S2_BENCH_TILE") {
            items.retain(|i| i.mgrs == tile);
        }
        items.truncate(n);
        assert!(!items.is_empty(), "no scenes for the bench bbox/window");
        let t = Thresholds::default();
        let readers: [(&str, Box<dyn SceneReader>); 3] = [
            ("windowed", Box::new(GdalReader)),
            ("bulk-cpu", Box::new(BulkReader { gpu: false })),
            ("bulk-gpu", Box::new(BulkReader { gpu: true })),
        ];
        let mut tot = [0.0f64; 3];
        for it in &items {
            eprint!("{} {}: ", it.mgrs, it.date);
            for (i, (name, r)) in readers.iter().enumerate() {
                let t0 = Instant::now();
                let (d, _) = detect_scene(&**r, it, bbox, true, &t, false).expect("detect");
                let dt = t0.elapsed().as_secs_f64();
                tot[i] += dt;
                eprint!("{name} {dt:.1}s/{} det · ", d.len());
            }
            eprintln!();
        }
        eprintln!("\n== totals over {} tiles ==", items.len());
        for (i, (name, _)) in readers.iter().enumerate() {
            eprintln!(
                "  {name:8} {:.1}s  ({:.1}s/tile)",
                tot[i],
                tot[i] / items.len() as f64
            );
        }
        eprintln!(
            "speedup vs windowed: bulk-cpu {:.2}× · bulk-gpu {:.2}×",
            tot[0] / tot[1],
            tot[0] / tot[2]
        );
    }

    // wide-area THROUGHPUT bench — the metric that matters for bulk: many full tiles
    // through a rayon pool (scene concurrency), wall-clock per reader. on a few-core
    // box the GPU's win is offloading decode from scarce CPUs (idle GPU) → all cores
    // free for the frozen detect compute. concurrency-capped to fit the 6 GB vGPU.
    //   S2_BENCH_BBOX=W,S,E,N [S2_BENCH_N=12] [S2_BENCH_C=4] [S2_BENCH_START/END] \
    //     cargo test -p s2e-cli --release --features gpu bench_throughput -- --ignored --nocapture
    #[test]
    #[ignore]
    fn bench_throughput() {
        crate::read::configure();
        let s = std::env::var("S2_BENCH_BBOX").expect("set S2_BENCH_BBOX=W,S,E,N");
        let v: Vec<f64> = s.split(',').map(|x| x.trim().parse().unwrap()).collect();
        let bbox = [v[0], v[1], v[2], v[3]];
        let go = |k: &str, d: &str| std::env::var(k).unwrap_or_else(|_| d.into());
        let (start, end) = (
            go("S2_BENCH_START", "2024-08-01"),
            go("S2_BENCH_END", "2024-08-31"),
        );
        let n: usize = go("S2_BENCH_N", "12").parse().unwrap();
        let c: usize = go("S2_BENCH_C", "4").parse().unwrap();
        let mut items =
            crate::stac::search(bbox, &start, &end, 100.0, "cdse").expect("stac search");
        if let Ok(tile) = std::env::var("S2_BENCH_TILE") {
            items.retain(|i| i.mgrs == tile);
        }
        items.truncate(n);
        assert!(!items.is_empty(), "no scenes for the bench bbox/window");
        let t = Thresholds::default();
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(c)
            .build()
            .unwrap();
        let readers: [(&str, Box<dyn SceneReader>); 3] = [
            ("windowed", Box::new(GdalReader)),
            ("bulk-cpu", Box::new(BulkReader { gpu: false })),
            ("bulk-gpu", Box::new(BulkReader { gpu: true })),
        ];
        eprintln!(
            "throughput: {} scenes · concurrency {} · {}..{}",
            items.len(),
            c,
            start,
            end
        );
        let mut tot = [0.0f64; 3];
        for (i, (name, r)) in readers.iter().enumerate() {
            use rayon::prelude::*;
            let t0 = Instant::now();
            let dets: usize = pool.install(|| {
                items
                    .par_iter()
                    .map(|it| {
                        detect_scene(&**r, it, it.bbox, true, &t, false)
                            .map(|(d, _)| d.len())
                            .unwrap_or(0)
                    })
                    .sum()
            });
            tot[i] = t0.elapsed().as_secs_f64();
            eprintln!(
                "  {name:8} {:.1}s  ({:.1}s/scene · {:.1} scenes/min · {} det)",
                tot[i],
                tot[i] / items.len() as f64,
                items.len() as f64 * 60.0 / tot[i],
                dets
            );
        }
        eprintln!(
            "throughput speedup vs windowed: bulk-cpu {:.2}× · bulk-gpu {:.2}×",
            tot[0] / tot[1],
            tot[0] / tot[2]
        );
    }
}
