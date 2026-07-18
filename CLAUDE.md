# s2e

The canonical Rust reference implementation for Sentinel-2 flare and methane-
plume detection. Shared L1C ingestion, native CloudSEN/MARS-S2L inference, flare
detection, methane retrieval, clustering and fleet execution live here once.

## Architecture

```
core/                       pure compute; no I/O
  detect.rs                   flare methodology
  plume.rs                    registration, model tensors, components, retrieval
  cluster.rs score.rs         persistent-site view and scoring
  coverage.rs geo.rs          cloud grid and geometry

cli/                        native application
  detect.rs                   shared scene execution + independent record commits
  record.rs                   canonical GeoJSON identity and atomic writes
  archive.rs                  publish unchanged records + derive Parquet views
  plume.rs                    MARS orchestration, quantification and result writing
  plume/chip.rs               L1C/CloudSEN chip preparation and spatial footprints
  plume/background.rs         temporal background selection and ranking
  plume/wind.rs               GEOS-FP download/cache/sample
  models.rs                   Candle-native checkpoint definitions
  read.rs stac.rs             GDAL and catalogue I/O
  view.rs                     DuckDB-backed derived view I/O
  main.rs                     CLI and cluster orchestration

wasm/                       the shared flare core for browser clients
gpu/                        optional CUDA/nvJPEG2000 reader, off by default
cloud/                      thin CloudFerro lifecycle around the native CLI
```

The `core/` boundary is typed slices in, results out. GDAL, HTTP, models, files and
object storage remain in `cli/`. GPU support is an optional crate and must not enter
normal CLI, core or WASM builds.

## Canonical data model

One valid GeoJSON `FeatureCollection` represents one detector analysis of one AOI
geometry and one Sentinel-2 scene under one method fingerprint:

```
observations/<area-hash>/<scene>/
  clouds-<method>.geojson
  flares-<method>.geojson
  plumes-<method>.geojson
assets/<area-hash>/<scene>/plumes-<method>.tif
```

The collection's foreign `analysis` member contains:

- detector, status, deterministic analysis id and method fingerprint;
- complete scene/source/radiometry metadata;
- original AOI geometry and properties;
- requested and actually processed footprints;
- detector-level values such as clear percentage, background, wind and score;
- optional references to pixel-level assets.

`features` contains zero or more spatial detections. An empty array is a successful
negative observation. Each connected methane component is a separate feature and
is quantified independently. Flares, plumes and clouds are separate records even
when computed together, so partial runs never mutate another detector's result.
Retrying the same method replaces the same deterministic path; a method change
creates a new record.

GeoJSON records and assets are authoritative. `views` creates disposable Parquet
indexes (`detections/`, `clouds/`, `plumes/`); `cluster` creates another derived
view. They may always be deleted and rebuilt from `observations/`.

## Methodology invariants

- L1C is canonical for both modes. `aws`/`cdse` L2A profiles exist only for flare
  comparison. Methane detection must reject L2A.
- `Thresholds::default()` is the historical validated compact-source L1C flare
  baseline recovered from burnoff history. Expose meaningful scalar overrides;
  do not add drifting presets.
- Flare size and radiance come from the combustion-hot component, not the loose
  spectral mask, which can flood across a warm facility.
- MARS background selection keeps the published nearest-date semantics and scores
  at most the first 20 qualifying clear scenes. Batching may improve I/O but must
  not change candidate order or membership.
- CloudSEN and MARS checkpoints are loaded directly with Candle and verified using
  the hashes in `models.rs`. Do not introduce a Python detector runtime.
- Plume components are retained separately. Retrieval alignment and methane
  enhancement may be shared, but flux and uncertainty are calculated per component.
- `cluster_detections` and scoring remain pure shared-core functions. Persistence
  uses distinct clear dates from the cloud grid and is a score term, not a hard
  count gate.

## Execution

For point/AOI L1C work, `detect --mode both` is the default. It performs one STAC
search, one 13-band chip read and one CloudSEN pass, then feeds flare and plume
branches. Larger flare polygons fall back to the full-AOI reader so the plume chip
cannot clip coverage. Whole-tile `--region` runs remain flare-only.

Every record is written to a same-directory `.part` and renamed atomically. A
record is cached only when its schema, detector, scene and method fingerprint all
match. Errors remain retryable `.err` files and successful commits remove them.
Positive probability rasters are committed before their referencing GeoJSON.

CloudFerro's `box.sh` only provisions, syncs, builds, shards, launches, gathers and
tears down. The Rust `archive` command publishes canonical records, `views` rebuilds
the disposable Parquet indexes, and `cluster` builds the cluster snapshot; the `etl`
repo owns their scheduled cadence. No detector-specific shell plugins or alternate
orchestration paths are allowed.

## Checks

```bash
cargo fmt --all -- --check
cargo test -p s2e-core -p s2e-cli -p s2e-wasm --no-default-features
bash -n cloud/box.sh
```

Network/model/GPU parity tests are ignored by default and require their documented
fixtures or environment variables. Keep the ordinary CPU/WASM suite dependency-
light and deterministic.
