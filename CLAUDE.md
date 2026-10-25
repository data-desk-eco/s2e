# s2e

An experimental Rust implementation of Sentinel-2 methane-plume detection,
with the flare detection it grew from. Data Desk's flares are burnoff's
(github.com/data-desk-eco/burnoff), clustered in the ETL's SQL; the flare
path here is kept for comparison and for `wasm/`. Shared L1C ingestion,
native CloudSEN/MARS-S2L inference, flare detection, methane retrieval and
clustering live here once. The detector only — the machines belong to
`data-desk`, the schedules to `etl`.

## Architecture

```
core/                       pure compute; no I/O
  detect.rs                   flare methodology
  plume.rs                    registration, model tensors, components, retrieval
  cluster.rs score.rs         persistent-site clustering and scoring
  review.rs                   plume-candidate triage verdicts
  coverage.rs geo.rs          cloud grid and geometry

cli/                        native application
  detect.rs                   shared scene execution + independent record commits
  record.rs                   canonical GeoJSON identity and atomic writes
  archive.rs                  publish unchanged records and coverage
  plume.rs                    MARS orchestration, quantification and result writing
  plume/chip.rs               L1C/CloudSEN chip preparation and spatial footprints
  plume/background.rs         temporal background selection and ranking
  plume/wind.rs               GEOS-FP download/cache/sample
  models.rs                   Candle-native checkpoint definitions
  read.rs stac.rs             GDAL and catalogue I/O
  view.rs                     DuckDB-backed clustering I/O
  review.rs                   plume-detection triage → ranked curation list
  main.rs                     CLI and cluster orchestration

  verify.rs                   run completeness: every feature scanned, no errors left
wasm/                       the shared flare core for browser clients
gpu/                        optional CUDA/nvJPEG2000 reader, off by default
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

GeoJSON records and assets are authoritative. The ETL job parses the plume
records into staging Parquet beside burnoff's flares, and publishes
`data-desk/detections/`, `data-desk/observations/` and `data-desk/flares/`
from both. Every one of those
is disposable and may be rebuilt from the private raw observations.

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
- `cluster` scores and names sites; it does not count days. The counts and the
  looks behind them are calculated where both tables are held, because that is
  the only place a numerator and a denominator can be made to cover the same
  window.
- Plume triage (`triage`) is a pure-core precision layer over the published plume
  detections, never the canonical records. Max probability is a floor only, never
  a ranking weight, and curation into
  `etl/providers/data-desk/burnoff/sql/valid-plumes.txt` stays human.

## Execution

For point/AOI L1C work, `detect --mode both` is the default. It performs one STAC
search, one 13-band chip read and one CloudSEN pass, then feeds flare and plume
branches. Larger flare polygons fall back to the full-AOI reader so the plume chip
cannot clip coverage. Whole-tile `--region` runs remain flare-only.

Every record is written to a same-directory `.part` and renamed atomically. A
record is cached only when its schema, detector, scene and method fingerprint all
match. Errors remain retryable `.err` files and successful commits remove them.
Positive probability rasters are committed before their referencing GeoJSON.

Every operation over the record layout is a subcommand, never a script elsewhere.
`archive` publishes canonical records, `cluster` groups the parsed detections into
sites and their membership, `verify` proves a run is complete and `coverage`
maintains the published coverage overlay. `review` scores the published plume
detections (wind consistency, fixed-offset recurrence, scene-day regimes,
cross-orbit backgrounds, magnitude prior, scene hygiene, optional OSM-line
collinearity against the probability assets) into a ranked candidate CSV for
valid-plume curation.

## What is not here

Fleets and schedules are somebody else's. `data-desk`'s `infra/fleet.sh` boots N
CloudFerro boxes, pushes a binary and a payload, launches, follows and tears down,
and knows nothing about this detector. The data-desk ETL no longer runs s2e:
it reads the plume records s2e wrote, which are fixed. A new run would pin a
tagged release, not a working tree.

The seam is `--shard I/N`, `verify` and `coverage`: enough for a generic
orchestrator to split, check and publish a run without ever parsing a record. Do
not add a shell orchestrator, a detector plugin or an alternate execution path
here; extend those three instead.

## Checks

```bash
cargo fmt --all -- --check
cargo test -p s2e-core -p s2e-cli -p s2e-wasm --no-default-features
```

Network/model/GPU parity tests are ignored by default and require their documented
fixtures or environment variables. Keep the ordinary CPU/WASM suite dependency-
light and deterministic.
