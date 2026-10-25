# s2e

A native Rust reference implementation for detecting gas flares and methane
plumes in Sentinel-2 imagery. Both modes share L1C scene search, CloudSEN masking
and georeferencing. Point and AOI runs default to both: a lit flare and an unlit
methane source are two states of the same facility.

s2e is experimental, and its focus is methane plumes. Data Desk's flare
detections come from [burnoff](https://github.com/data-desk-eco/burnoff), a
DuckDB extension, and the ETL clusters them into sites in SQL. The flare path
here remains for comparison and for `wasm/`.

There is no Python detector runtime. Candle loads the published MARS-S2L and
CloudSEN PyTorch checkpoints directly and verifies their pinned SHA-256 hashes.

```
core/   pure flare/plume compute, retrieval, clustering and geometry
cli/    STAC + GDAL I/O, native models, GeoJSON records and clustering
wasm/   the shared flare methodology exposed to browser clients
gpu/    an optional CUDA/nvJPEG2000 reader, off by default
```

This repository is the detector and nothing else. The machines it runs on belong
to [data-desk](https://github.com/data-desk-eco/data-desk) (`infra/fleet.sh`), and
the schedule on which they run belongs to
[etl](https://github.com/data-desk-eco/etl) (`s2e/`).

## Why L1C

L1C is canonical for both detectors. The original burnoff method preferred L1C
because atmospheric correction can clip the strongest SWIR thermal signal. The
later L2A path was an availability compromise for a public COG archive. CloudFerro
exposes L1C directly, so both modes now use it. `aws` and `cdse` retain L2A only as
explicit flare-comparison profiles.

## CLI

```bash
cargo build --release -p s2e-cli
target/release/s2e models

# Both signals, sharing one chip and CloudSEN pass.
target/release/s2e detect \
  --aoi aoi/uk-gas-import-terminals.geojson \
  --start 2026-01-01 --end 2026-07-17 --out out/uk

# Independent modes remain independently resumable.
target/release/s2e detect --mode plumes --bbox 53.79,39.35,53.81,39.37
target/release/s2e detect --mode flares --region 51.4,25.8,51.7,26.1

# Resolve a campaign's scenes once, into a manifest the whole fleet reads. Detection
# then makes no catalogue requests at all, and a resume makes none either.
target/release/s2e discover \
  --aoi aoi/uk-gas-import-terminals.geojson --source cdse-l1c \
  --start 2026-01-01 --end 2026-07-17 --jobs 4 --out scenes.ndjson.gz
target/release/s2e detect --aoi aoi/uk-gas-import-terminals.geojson \
  --scenes scenes.ndjson.gz --start 2026-01-01 --end 2026-07-17 --out out/uk

# Publish the canonical records unchanged. Turning them into Parquet is etl's
# job (`etl/providers/data-desk/s2e/views`), so there is no `views` subcommand here.
target/release/s2e archive --input out/uk --destination s3://bucket

# Cluster the parsed detections into persistent flare sites, and say which
# detection joined which site. ETL reads both back and publishes the tables.
target/release/s2e cluster \
  --detections staging/flares.parquet --clouds staging/clouds.parquet \
  --clusters staging/clusters.parquet --members staging/members.parquet
```

## Discovery

`detect` searches the catalogue once per AOI feature as it reaches it. That is fine
for a handful of sites and the wrong shape for a campaign: the searches are serial,
they all happen before any reading, each L1C search triggers a second for the L2A twin
that carries the cloud mask, every fleet member repeats the work for its own shard, and
a resume repeats all of it again. A large AOI over the full archive reaches tens of
thousands of requests, and the endpoint sheds bursts past about seven.

`discover` does that work once. It batches the AOI into as few searches as the endpoint
takes — the features go as one MultiPolygon, with overlapping envelopes merged because
GEOS refuses to intersect against overlapping parts — and writes every scene it finds
as a gzipped NDJSON manifest: a header line, then one scene per line. The cloud
threshold is deliberately not applied, so one manifest serves any `--cloud`, every
shard, and both detectors.

A long campaign splits with `--jobs N`: the window becomes N sub-windows resolved at
once. Requests share one process-wide pace, so this overlaps the waiting rather than
asking faster, and the endpoint sees the same steady stream either way. Each job holds
its own scene list, so raise it on a machine with the memory for it — a nine-year
European campaign is a gigabyte or so per job — and not on a shared control plane.

`detect --scenes` then selects each feature's scenes from that file using the same
envelope test discovery used, so a manifest run and a per-feature search return the
same scenes. The manifest is checked rather than trusted: a run whose source, AOI or
window it does not cover is refused, and the message says which.

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

`archive` copies GeoJSON and raster assets unchanged. The records stop there: the
DuckDB transforms that parse the plume records into staging Parquet live in the
ETL repository (`etl/providers/data-desk/s2e/sql`), beside burnoff's flare
staging. The ETL no longer calls `cluster`; its SQL sites reproduce it. None of
the Parquet products is another authoritative detection format.

## Validation

- Native MARS-S2L probability agrees with published PyTorch inference within
  `2e-5` on the parity fixture; CloudSEN produces the same class map.
- On known plume `T_EMIT_227` (2024-10-25), Rust reproduces the published score and
  background; flux differs by 0.17% and uncertainty by 0.4%.
- Over the known Ras Laffan archive, strict L1C found 49 scene detections versus
  45 for L2A while both resolve the same 16 persistent sites.

```bash
cargo test -p s2e-core -p s2e-cli -p s2e-wasm --no-default-features
```

## Running at scale

Bulk runs happen on a CloudFerro fleet in WAW3-2, next to the Copernicus archive.
Nothing about that fleet lives here. A tagged release publishes a Linux binary to
the archive, and the `etl` repo fetches it, spreads it over as many boxes as it wants
and runs it:

```bash
make s2e-aoi NAME=lng AOI='kind=lng_terminal,status=operating'
make s2e-run NAME=lng ARGS="--start 2026-01-01 --end 2026-07-17"
```

Three flags are what make that possible, and they are the only concession this CLI
makes to being run by something else:

- `--shard I/N` takes member `I` of an N-way split of `--aoi`, so every box works a
  balanced slice of one shared file and no orchestrator has to cut it up.
- `verify` proves a run is complete — every requested feature has a durable record,
  no retryable `.err` is left — and exits non-zero if it is not.
- `coverage` merges the scanned AOI into the map's published coverage overlay.

Each of these needs to know how a record is laid out and how a feature is named, so
each belongs here rather than in a shell script somewhere else.

The optional GPU reader is a development box rather than a fleet, since it needs a
`--features gpu` build from source. `gpu/cloud-init.yaml` is its recipe:

```bash
FLEET_NAME=s2e-gpu FLEET=1 CF_FLAVOR=vm.l40s.1 FLEET_OS='Ubuntu 22.04 NVIDIA' \
  CF_USERDATA=$PWD/gpu/cloud-init.yaml ~/data-desk/infra/fleet.sh up
```

Then run the parity gate on it — nvJPEG2000 against GDAL/OpenJPEG over real scenes,
which must agree byte for byte:

```bash
S2_PARITY_BBOX=W,S,E,N cargo test --release -p s2e-cli --features gpu parity -- --ignored --nocapture
```
