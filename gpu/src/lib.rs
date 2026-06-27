//! safe rust wrapper over the nvjpeg2000 cuda shim (src/shim.cu). the crate's whole
//! job: decode a Sentinel-2 B12 JP2 codestream to a `Vec<u16>` on the GPU. lossless
//! JP2 → identical pixels to GDAL/OpenJPEG, so the gpu full-tile path stays byte-for-
//! byte with the cpu windowed path. all CUDA linkage lives here behind `--features gpu`.

use std::os::raw::{c_int, c_uchar, c_ushort};

extern "C" {
    fn s2g_decode(data: *const c_uchar, len: usize, out: *mut *mut c_ushort, w: *mut c_int, h: *mut c_int) -> c_int;
    fn s2g_free(p: *mut c_ushort);
}

/// decode a single-component 16-bit JP2 codestream → (pixels row-major, width, height).
pub fn decode_b12(bytes: &[u8]) -> Result<(Vec<u16>, usize, usize), String> {
    let (mut out, mut w, mut h): (*mut c_ushort, c_int, c_int) = (std::ptr::null_mut(), 0, 0);
    let rc = unsafe { s2g_decode(bytes.as_ptr(), bytes.len(), &mut out, &mut w, &mut h) };
    if rc != 0 || out.is_null() { return Err(format!("nvjpeg2000 decode failed (rc={rc})")); }
    let n = w as usize * h as usize;
    let pixels = unsafe { std::slice::from_raw_parts(out, n) }.to_vec();
    unsafe { s2g_free(out) };
    Ok((pixels, w as usize, h as usize))
}
