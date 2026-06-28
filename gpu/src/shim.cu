// nvjpeg2000 decode shim — the only CUDA in the workspace. decodes Sentinel-2 SWIR
// JP2 codestreams (single 16-bit component each) to host uint16 buffers. JP2 is
// lossless (reversible 5/3 wavelet), so this returns the SAME integer pixels as
// OpenJPEG/GDAL → byte-for-byte parity with the cpu path's core::detect_block.
//
// the bulk full-tile path hands a scene's spectral bands (B12, B11, B8A) to one call.
// each band decodes with its OWN handle/state/stream, fully torn down before the next
// (the proven one-decode-per-handle pattern, just looped) — so peak device memory is a
// single 30MP decode (~0.9 GB scratch), which fits the 6 GB L40S vGPU; reusing one
// state across bands fails on this driver. the big win is whole-tile decode replacing
// the windowed per-block reads, not cross-band overlap.
#include <nvjpeg2k.h>
#include <cuda_runtime.h>
#include <cstdlib>
#include <cstdio>

// decode one single-component 16-bit JP2 codestream → *out (malloc'd host uint16,
// caller frees via s2g_free), dims in *w/*h. 0 on success, distinct nonzero per step.
static int decode_one(const unsigned char *data, size_t len, unsigned short **out, int *w, int *h) {
    nvjpeg2kHandle_t handle = nullptr;
    nvjpeg2kDecodeState_t state = nullptr;
    nvjpeg2kStream_t stream = nullptr;
    cudaStream_t cstream = nullptr;
    unsigned short *d_img = nullptr, *h_img = nullptr;
    nvjpeg2kImageComponentInfo_t comp;
    nvjpeg2kImage_t img;
    void *planes[1];
    size_t pitches[1];
    size_t bytes = 0;
    unsigned W = 0, H = 0;
    int rc = 1;
    *out = nullptr;

    if (nvjpeg2kCreateSimple(&handle) != NVJPEG2K_STATUS_SUCCESS) goto done;
    if (nvjpeg2kDecodeStateCreate(handle, &state) != NVJPEG2K_STATUS_SUCCESS) goto done;
    if (nvjpeg2kStreamCreate(&stream) != NVJPEG2K_STATUS_SUCCESS) goto done;
    if (nvjpeg2kStreamParse(handle, data, len, 0, 0, stream) != NVJPEG2K_STATUS_SUCCESS) { rc = 2; goto done; }
    if (nvjpeg2kStreamGetImageComponentInfo(stream, &comp, 0) != NVJPEG2K_STATUS_SUCCESS) { rc = 2; goto done; }

    W = comp.component_width; H = comp.component_height;
    bytes = (size_t)W * H * sizeof(unsigned short);
    pitches[0] = (size_t)W * sizeof(unsigned short);
    if (cudaMalloc(&d_img, bytes) != cudaSuccess) { rc = 3; goto done; }
    if (cudaStreamCreate(&cstream) != cudaSuccess) { rc = 3; goto done; }

    planes[0] = d_img;
    img.pixel_data = planes; img.pixel_type = NVJPEG2K_UINT16;
    img.pitch_in_bytes = pitches; img.num_components = 1;
    if (nvjpeg2kDecode(handle, state, stream, &img, cstream) != NVJPEG2K_STATUS_SUCCESS) { rc = 4; goto done; }
    if (cudaStreamSynchronize(cstream) != cudaSuccess) { rc = 5; goto done; }

    h_img = (unsigned short *)malloc(bytes);
    if (!h_img) { rc = 6; goto done; }
    if (cudaMemcpy(h_img, d_img, bytes, cudaMemcpyDeviceToHost) != cudaSuccess) { free(h_img); h_img = nullptr; rc = 6; goto done; }
    *out = h_img; *w = (int)W; *h = (int)H; rc = 0;

done:
    // surface the opaque CUDA reason on failure (e.g. a driver/runtime version skew).
    if (rc) fprintf(stderr, "s2g decode rc=%d cuda='%s'\n", rc, cudaGetErrorString(cudaGetLastError()));
    if (cstream) cudaStreamDestroy(cstream);
    if (d_img) cudaFree(d_img);
    if (stream) nvjpeg2kStreamDestroy(stream);
    if (state) nvjpeg2kDecodeStateDestroy(state);
    if (handle) nvjpeg2kDestroy(handle);
    return rc;
}

// decode n codestreams → out[i] (w[i]*h[i]). returns 0 iff every image decoded.
extern "C" int s2g_decode_batch(const unsigned char *const *data, const size_t *len, int n,
                                unsigned short **out, int *w, int *h) {
    for (int i = 0; i < n; i++) out[i] = nullptr;
    for (int i = 0; i < n; i++) {
        int rc = decode_one(data[i], len[i], &out[i], &w[i], &h[i]);
        if (rc != 0) { for (int j = 0; j < n; j++) if (out[j]) { free(out[j]); out[j] = nullptr; } return rc; }
    }
    return 0;
}

extern "C" void s2g_free(unsigned short *p) { free(p); }
