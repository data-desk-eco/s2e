// nvjpeg2000 decode shim — the only CUDA in the workspace. decodes one Sentinel-2
// B12 JP2 codestream (single 16-bit component) to a host uint16 buffer. JP2 is
// lossless (reversible 5/3 wavelet), so this returns the SAME integer pixels as
// OpenJPEG/GDAL → byte-for-byte parity with the cpu path's core::detect_block.
//
// one handle per call (create/destroy): simple and thread-safe under the rayon
// scene fan-out. batched decode (many codestreams per call) is the throughput
// lever to layer on once parity is locked — see gpu-plan.md.
#include <nvjpeg2k.h>
#include <cuda_runtime.h>
#include <cstdlib>

// decode `data[0..len)` → *out (malloc'd host uint16, *w * *h, caller frees via
// s2g_free), dims in *w/*h. returns 0 on success, nonzero on any failure.
extern "C" int s2g_decode(const unsigned char *data, size_t len,
                          unsigned short **out, int *w, int *h) {
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

    if (nvjpeg2kCreateSimple(&handle) != NVJPEG2K_STATUS_SUCCESS) goto done;
    if (nvjpeg2kDecodeStateCreate(handle, &state) != NVJPEG2K_STATUS_SUCCESS) goto done;
    if (nvjpeg2kStreamCreate(&stream) != NVJPEG2K_STATUS_SUCCESS) goto done;
    if (nvjpeg2kStreamParse(handle, data, len, 0, 0, stream) != NVJPEG2K_STATUS_SUCCESS) goto done;
    if (nvjpeg2kStreamGetImageComponentInfo(stream, &comp, 0) != NVJPEG2K_STATUS_SUCCESS) goto done;

    W = comp.component_width;
    H = comp.component_height;
    bytes = (size_t)W * H * sizeof(unsigned short);
    pitches[0] = (size_t)W * sizeof(unsigned short);

    if (cudaMalloc(&d_img, bytes) != cudaSuccess) goto done;
    if (cudaStreamCreate(&cstream) != cudaSuccess) goto done;

    planes[0] = d_img;
    img.pixel_data = planes;
    img.pixel_type = NVJPEG2K_UINT16;
    img.pitch_in_bytes = pitches;
    img.num_components = 1;

    if (nvjpeg2kDecode(handle, state, stream, &img, cstream) != NVJPEG2K_STATUS_SUCCESS) goto done;
    if (cudaStreamSynchronize(cstream) != cudaSuccess) goto done;

    h_img = (unsigned short *)malloc(bytes);
    if (!h_img) goto done;
    if (cudaMemcpy(h_img, d_img, bytes, cudaMemcpyDeviceToHost) != cudaSuccess) { free(h_img); h_img = nullptr; goto done; }

    *out = h_img;
    *w = (int)W;
    *h = (int)H;
    rc = 0;

done:
    if (cstream) cudaStreamDestroy(cstream);
    if (d_img) cudaFree(d_img);
    if (stream) nvjpeg2kStreamDestroy(stream);
    if (state) nvjpeg2kDecodeStateDestroy(state);
    if (handle) nvjpeg2kDestroy(handle);
    return rc;
}

extern "C" void s2g_free(unsigned short *p) { free(p); }
