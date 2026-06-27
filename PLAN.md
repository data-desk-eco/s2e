# plan: rust core → native CLI + WASM, CloudFerro pipeline alongside

turn s2-flares into a minimal Rust workspace: one pure methodology core that
compiles both to a fast native CLI and to WASM for the browser/Lambda consumers,
killing the JS/JS-bulk implementation split (and the parity-drift it forces). the
existing `detect.js`-takes-typed-arrays / I/O-in-`cog.js` boundary is exactly the
native-vs-WASM seam — port along it.

**hard rule: the frozen methodology must not drift.** every stage gates on a
parity harness (below) before the JS lineage is touched. preserve byte-for-byte:
the spectral mask, DEFAULTS/LOOSE thresholds, the score formula, the spectral
glint discriminator, the cluster `id` hash, presence==done resumability, and the
parquet hive layout (`flares/preset=…/mgrs=…/date=…/data.parquet`).

## workspace layout (cargo)

- **`core/`** — pure, no I/O, `#![no_std]`-friendly, compiles to WASM. ports
  `lib/`: detect + thresholds, cluster, score + glint geometry, coverage, geo
  (UTM/WGS84, `epsgFromMgrs`). slices in, detections out. ports the JS unit tests.
- **`cli/`** (native bin) — GDAL-async equivalent via the `gdal` crate: windowed
  `/vsis3/eodata` JP2 reads (JP2OpenJPEG, **subtract the N0400 BOA_ADD_OFFSET** on
  spectral bands, not SCL) + COG byte-range reads; STAC search (`aws` + `cdse`
  profiles); `runAOI` pipeline; per-scene fan-out via `rayon`; CSV + parquet out
  via `arrow`/`parquet`. reproduces `cli.js` *and* `cf-run.js` (incl. `--bbox` /
  `--aoi` / `--preset` / `--source aws|cdse`, per-scene `<mgrs>_<date>` files,
  resumable).
- **`wasm/`** — `wasm-bindgen` shim exposing core (detectBlock, cluster, score)
  to JS for burnoff/gaslight/Lambda; built with `wasm-pack`. I/O stays JS glue
  (browser fetch byte-ranges, pass typed arrays in) — mirrors today's split.

## stages

1. **scaffold + core port.** workspace; port `core/` 1:1 from JS with tests.
2. **parity harness (gate).** golden scenes — reuse the JP2/COG-verified set
   (e.g. the 3228=3228 scene). Rust core vs the JS reference → identical detection
   counts/fields. nothing downstream proceeds until green.
3. **native CLI.** GDAL I/O + STAC + fan-out + parquet; match `cli.js`/`cf-run.js`
   output on a real AOI. cross-compile musl static (or build on the box).
4. **WASM + consumer cutover.** build `wasm/`, wire burnoff/gaslight/Lambda to it,
   smoke-test, then retire `lib/` JS, `cog-gdal.js`, vendored geotiff.
5. **CloudFerro.** `box.sh run` ships+runs the static binary instead of
   `node cf-run.js`; `cloud-init.yaml` drops node/geotiff for the binary (+GDAL).
   up/archive/pull/down/publish, eodata per-VM creds, DuckDB+parquet archive —
   **unchanged**. the box stays disposable; the S3 archive persists.
6. **prune.** delete the JS lineage once parity holds and consumers are migrated.
   target end state: `core/ cli/ wasm/ cloudferro/ aoi/` + Cargo workspace.

## non-goals / keep

- don't reimplement raster decode — bind libgdal (JP2) and the JS GeoTIFF decoder
  (browser) as today; only the *compute* moves to Rust/WASM.
- DuckDB stays the analytics/scoring/archive layer; the hive parquet scheme and
  the web-map `publish` path are untouched.
- a later, optional egress-zero step (Sentinel Hub batch / openEO per-pixel
  reduction near the archive) is out of scope here — note it, don't build it.
