# gpu bulk flare detection — implementation plan

map flaring *everywhere*, not just catalogued sites. this plan adds a GPU full-tile
detection path (nvJPEG2000 decode + on-device prescreen) alongside the existing CPU
windowed path, sharing one methodology core, provisioned and run through `box.sh` on
a CloudFerro GPU box. it is a handoff doc for the implementing agent.

## the one principle: `core/` stays the single source of truth

the frozen methodology — spectral mask, background contrast, NHI-SWIR/saturation,
block detection, scoring, clustering, the cluster `id` hash — lives once in `core/`
and is **not** reimplemented in CUDA. the GPU path does exactly the two things the CPU
reader already does — decode pixels, and a coarse hot-pixel prescreen — then calls the
*same* `core::detect_block`. everything downstream (CSV, archive rollup, cluster view,
score) is shared and untouched.

minimal duplication, stated precisely: the GPU stage reimplements only a **recall-safe
threshold gate** (the `any_hot` / 8× overview prescreen that `cli/read.rs` already
does), never the detection *decision*. `core/` makes every real decision on both paths.
the gate must be a strict superset of the CPU mask's acceptance set (e.g. `B12 ≥` the
hot floor) so it can never drop a detection `core/` would have kept.

## why this is also the fast design (no perf sacrificed for non-duplication)

- the bottleneck is JP2 *decode*, not the detection math. measured on the CPU box:
  CPU-bound on OpenJPEG decode at ~34 scenes/min; the mask/score is cheap and is
  usually skipped by the overview early-out. put decode on the GPU (nvJPEG2000 ≈
  5–10× OpenJPEG, *batched*) and the only expensive thing has moved.
- keep the precise methodology on CPU but over **sparse candidates only** → trivial
  CPU cost. overlap it with GPU decode (producer/consumer queue) so the GPU never
  stalls waiting on CPU detection.
- transfer only candidate blocks device→host — the GPU prescreen discards empty tiles
  (the vast majority), so there is no full ~120 MB/tile round-trip.

the GPU does what GPUs are good at (bulk lossless decode + a per-pixel threshold); the
CPU keeps the sparse, must-not-drift logic. width comes from batching, not from porting
the methodology.

## parity is exact, not approximate

Sentinel-2 JP2 is **lossless** (reversible 5/3 wavelet), so nvJPEG2000 and OpenJPEG
decode to identical integer pixels → identical `core::detect_block` output. the GPU path
is therefore **byte-for-byte parity** with the CPU path. ship a parity test that asserts
GPU detections == CPU detections over sample tiles — this extends the existing
frozen-methodology gate to the GPU path and is a hard CI guard against drift.

## reuse map — shared vs new

**shared, unchanged:** all of `core/`; `cli/src/stac.rs` (search); the harmonisation
offset; the CSV writer; `cli/src/view.rs` (archive rollup + cluster view); `box.sh`
`archive`/`pull`/`publish`/`cluster`.

**refactor for sharing** (no behaviour change — CPU parity test stays green): lift the
per-scene loop in `cli/src/read.rs` behind a reader seam:

```
struct Candidate { meta: BlockMeta, b12: Vec<u16>, b11: Vec<u16>,
                   b8a: Option<Vec<u16>>, scl: Option<Vec<u8>> }   // exactly detect_block's inputs
trait SceneReader { fn candidates(&self, item: &Item, region: [f64;4], t: &Thresholds)
                        -> Result<(Vec<Candidate>, bool /*cloud_free*/), String>; }
```

- `GdalReader` = today's `read.rs` logic verbatim (windowed reads + overview prescreen
  + lazy aux bands), now expressed as a `SceneReader`.
- shared driver: `candidates → core::detect_block → detections → CSV`. identical for both
  readers; this is where the fan-out, dedup-by-canonical-block, and CSV write live.

**new, GPU-only** (behind a `gpu` cargo feature, isolated in a new `gpu/` crate so CPU
and wasm builds never pull a CUDA dep): `GpuReader` implementing `SceneReader`.

## the `gpu/` crate (feature-gated, CUDA isolated)

- a thin CUDA/C++ shim compiled by `build.rs` (nvcc), exposed over a C ABI and FFI'd
  from Rust. all CUDA stays in this one crate behind `--features gpu`; `core`, default
  `cli`, and `wasm` builds are unaffected and stay dependency-light.
- shim responsibilities:
  1. nvJPEG2000 **batched** decode of B12 codestream bytes (batch many scenes per call
     to amortise launch overhead — the throughput lever).
  2. a one-line prescreen kernel: flag blocks containing any `B12 ≥` hot floor
     (recall-safe superset of the mask).
  3. return candidate block coordinates + their B12 pixels host-side.
- **two-tier prescreen** mirroring the CPU 8× overview: batch-decode B12 at a reduced
  nvJPEG2000 resolution level first, threshold, then full-decode only scenes/regions
  that pass. most tiles are empty → cheap reject at low res.
- **aux bands** (B11/B8A/SCL) for candidate blocks: read the small candidate windows via
  the existing `GdalReader` windowed path (cheap, sparse) — do **not** GPU-decode whole
  aux tiles. reuse, don't reimplement. (later toggle: batch-GPU-decode B11 if a tile is
  flare-dense.)
- **byte source / geometry:** fetch raw `.jp2` object bytes from `/vsis3/eodata` with the
  same per-VM creds → feed nvJPEG2000. geotransform/EPSG from a cheap GDAL `open` (or
  `core::geo` from the MGRS id) — `open` is not the cost, decode is.

## wide-area work model (tiles, not points)

the archive is already AOI-agnostic, so full-tile detections drop into the same
`detections/mgrs=…/data.parquet` layout with **zero schema change**. change only the
iteration unit:

- new input `--region W,S,E,N` (and/or `--tiles <mgrs,…>` / global) → enumerate
  intersecting MGRS tiles → STAC search per tile × date window → full-tile decode+detect.
- `--aoi` (windowed, point sites) remains the CPU default; `--region` (full-tile) is the
  GPU target. flags are orthogonal — the driver simply passes the whole-tile bbox as the
  `region` to the GPU reader.

## `box.sh`: GPU box support (minimal)

`box.sh` already parameterises `FLAVOR`/`IMAGE`/`RATE` via env. add:

- `cloud/cloud-init-gpu.yaml`: NVIDIA driver + CUDA toolkit + nvJPEG2000 (CUDA
  redistributable / DALI) + clang/gdal/duckdb + clone + `cargo build --release -p
  s2-flares-cli --features gpu`.
- `box.sh`: select the cloud-init file + flavor with a `GPU=1` switch (or
  `CLOUD_INIT=cloud-init-gpu.yaml FLAVOR=<gpu flavor>`); pass `--gpu` through `run`; set
  `RATE` to the GPU flavor €/h so `cost` stays accurate. `archive`/`pull`/`down`
  unchanged.

before coding the cloud-init, the agent must verify in-region: `openstack flavor list |
grep -i gpu` to pick a CUDA-capable flavor, confirm a CUDA base image or a driver-install
path on WAW3-2, and confirm nvJPEG2000 is installable there.

## what the implementing agent executes

1. **SceneReader refactor** — pure restructure of `read.rs`; CPU parity tests stay green.
2. **`gpu/` crate + CUDA shim** — nvJPEG2000 batched decode + prescreen kernel; builds
   under `--features gpu` on a CUDA box.
3. **CLI wiring** — add `--gpu` (reader select) and `--region`/`--tiles` (work model);
   driver picks reader + work-list.
4. **`cloud-init-gpu.yaml` + `box.sh` GPU switch.**
5. **provision + parity** — `GPU=1 FLAVOR=<gpu> ./box.sh up`; build on box; parity-test
   one tile GPU vs CPU and assert identical detections.
6. **run** — `./box.sh run --gpu --region <test bbox> --start … --end …`; watch
   throughput (target tiles/min ≫ the CPU baseline; confirm GPU stays saturated).
7. **finish** — `./box.sh archive && ./box.sh pull && ./box.sh down`. same artifacts;
   the web map and cluster view consume them unchanged.

## risks / verify-first

- CloudFerro GPU flavor names + nvJPEG2000 availability in-region (research confirms GPU
  lines exist but didn't enumerate) — verify via `openstack` and a boot test.
- CUDA toolkit ↔ nvJPEG2000 ↔ driver version compatibility on the chosen image.
- keep **all** CUDA inside the `build.rs`-compiled shim behind the `gpu` feature; never
  leak it into `core`/default `cli`/`wasm`. `cudarc` optional, only for device mgmt.
- **connected-components stays on CPU in `core/`** — do not port it to the GPU. candidates
  are sparse, so CPU cost is negligible and the methodology stays single-sourced. this is
  the line that keeps "minimal duplication" honest.
- cost: GPU €/h ≫ CPU; justified only by full-tile decode volume. the CPU windowed path
  stays the default for site monitoring — both paths are kept, by design.

## cost shape

GPU wins wall-clock on full-tile, decode-heavy work: it decodes far more pixels than the
windowed CPU path but ~5–10× faster per pixel and overlaps CPU detection. €/GPU-hour is
roughly flat — size the run to the region. egress ≈ nil (everything stays in-region;
only the final CSV/parquet pull leaves).
