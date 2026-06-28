//! safe rust wrapper over the nvjpeg2000 cuda shim (src/shim.cu). the crate's whole
//! job: decode a scene's Sentinel-2 SWIR JP2 codestreams to `Vec<u16>` on the GPU in
//! one call (the shim decodes them one after another, peak memory = a single 30MP
//! decode so it fits the 6 GB L40S vGPU). lossless JP2 → identical pixels to
//! GDAL/OpenJPEG, so the gpu bulk path stays byte-for-byte with the cpu windowed
//! path. all CUDA linkage lives here behind `--features gpu`.

use std::os::raw::{c_int, c_uchar, c_ushort};

extern "C" {
    fn s2g_decode_batch(data: *const *const c_uchar, len: *const usize, n: c_int,
        out: *mut *mut c_ushort, w: *mut c_int, h: *mut c_int) -> c_int;
    fn s2g_free(p: *mut c_ushort);
}

/// decode many single-component 16-bit JP2 codestreams in one call →
/// one (pixels row-major, width, height) per input, in order.
pub fn decode_batch(streams: &[&[u8]]) -> Result<Vec<(Vec<u16>, usize, usize)>, String> {
    let n = streams.len();
    let ptrs: Vec<*const c_uchar> = streams.iter().map(|s| s.as_ptr()).collect();
    let lens: Vec<usize> = streams.iter().map(|s| s.len()).collect();
    let mut out = vec![std::ptr::null_mut::<c_ushort>(); n];
    let (mut w, mut h) = (vec![0 as c_int; n], vec![0 as c_int; n]);
    let rc = unsafe {
        s2g_decode_batch(ptrs.as_ptr(), lens.as_ptr(), n as c_int, out.as_mut_ptr(), w.as_mut_ptr(), h.as_mut_ptr())
    };
    if rc != 0 { return Err(format!("nvjpeg2000 batch decode failed (rc={rc})")); }
    let mut res = Vec::with_capacity(n);
    for i in 0..n {
        if out[i].is_null() { return Err("nvjpeg2000 null output".into()); }
        let cnt = w[i] as usize * h[i] as usize;
        let pixels = unsafe { std::slice::from_raw_parts(out[i], cnt) }.to_vec();
        unsafe { s2g_free(out[i]) };
        res.push((pixels, w[i] as usize, h[i] as usize));
    }
    Ok(res)
}
