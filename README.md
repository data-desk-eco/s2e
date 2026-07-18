# s2-flares

A native Rust reference implementation for detecting gas flares and methane
plumes in Sentinel-2 imagery. Both modes share L1C scene search, CloudSEN masking,
georeferencing and CloudFerro orchestration. Point and AOI runs default to both:
a lit flare and an unlit methane source are two states of the same facility.

There is no Python detector runtime. Candle loads the published MARS-S2L and
CloudSEN PyTorch checkpoints directly and verifies their pinned SHA-256 hashes.

```
core/   pure flare/plume compute, retrieval, clustering and geometry
cli/    STAC + GDAL I/O, native models, GeoJSON records and archive views
wasm/   the shared flare methodology exposed to browser clients
cloud/  a thin CloudFerro fleet lifecycle around the same CLI
```

## Why L1C

L1C is canonical for both detectors. The original burnoff method preferred L1C
because atmospheric correction can clip the strongest SWIR thermal signal. The
later L2A path was an availability compromise for a public COG archive. CloudFerro
exposes L1C directly, so both modes now use it. `aws` and `cdse` retain L2A only as
explicit flare-comparison profiles.

## CLI

```bash
cargo build --release -p s2-flares-cli
target/release/s2-flares models

# Both signals, sharing one chip and CloudSEN pass.
target/release/s2-flares detect \
  --aoi aoi/uk-gas-import-terminals.geojson \
  --start 2026-01-01 --end 2026-07-17 --out out/uk

# Independent modes remain independently resumable.
target/release/s2-flares detect --mode plumes --bbox 53.79,39.35,53.81,39.37
target/release/s2-flares detect --mode flares --region 51.4,25.8,51.7,26.1

# Publish the canonical records unchanged and rebuild disposable Parquet views.
target/release/s2-flares archive --input out/uk --destination s3://bucket --views

# Derive a flare-site view for another date window.
target/release/s2-flares cluster \
  --archive 's3://bucket/detections/**/*.parquet' --out clusters.parquet
```

Sources are `aws-l1c` (default), `cdse-l1c`, `aws` and `cdse`. Fixed `--wind-u`
and `--wind-v` values make plume runs offline and reproducible; otherwise the
acquisition-hour GEOS-FP field is downloaded atomically into a bounded cache.
Background selection retains the published nearest-date, first-20-clear-scene
semantics while loading candidates in small parallel batches.

## Canonical records

The source of truth is a valid GeoJSON `FeatureCollection` for one detector,
target geometry, Sentinel-2 scene and methodology fingerprint:

```
out/observations/<area-hash>/<scene>/
  clouds-<method>.geojson
  flares-<method>.geojson
  plumes-<method>.geojson
out/assets/<area-hash>/<scene>/
  plumes-<method>.tif                 # positive probability raster, when present
```

Each collection carries the original AOI geometry and properties, requested and
processed footprints, scene/source metadata, model or threshold fingerprint and
analysis status. `features` contains zero or more spatial detections; an empty
array is a successful negative observation. Multiple connected plume components
are separate features with independently calculated flux and uncertainty.

Detector records are deliberately independent. A flare-only run never updates a
plume result, and a later plume run never rewrites the flare record. A changed
methodology gets a new deterministic filename; retrying the same methodology
idempotently commits the same path. Combined runs share computation in memory but
retain this clean persistence boundary.

`archive` copies GeoJSON and raster assets unchanged. With `--views`, DuckDB
rebuilds `detections/`, `clouds/` and `plumes/results.parquet` as disposable query
indexes. `clusters/` is likewise derived; none of the Parquet products is another
authoritative detection format.

## Validation

- Native MARS-S2L probability agrees with published PyTorch inference within
  `2e-5` on the parity fixture; CloudSEN produces the same class map.
- On known plume `T_EMIT_227` (2024-10-25), Rust reproduces the published score and
  background; flux differs by 0.17% and uncertainty by 0.4%.
- Over the known Ras Laffan archive, strict L1C found 49 scene detections versus
  45 for L2A while both resolve the same 16 persistent sites.

```bash
cargo test -p s2-flares-core -p s2-flares-cli -p s2-flares-wasm --no-default-features
```

## CloudFerro

`cloud/box.sh` provisions and shards the fleet, rsyncs this implementation, runs
the same binary with `--source cdse-l1c`, gathers its immutable records and calls
the native `archive` command on the head:

```bash
cloud/box.sh launch --mode both --aoi aoi/uk-gas-import-terminals.geojson \
  --start 2026-01-01 --end 2026-07-17
cloud/box.sh watch
cloud/box.sh verify
cloud/box.sh archive
cloud/box.sh down
```
