# s2-flares

The canonical Sentinel-2 SWIR flare-detection methodology core, as a minimal Rust
workspace: one pure methodology core that compiles both to a fast native CLI and to
WebAssembly for the browser. It supersedes the openflaring/JS lineage — detector,
glint geometry, the vision-validated quality score, clustering, and the bulk
collector all live here, once, in Rust.

The frozen methodology must not drift. The pure compute is ported 1:1 from the
retired JS `lib/`, and the JS unit suites are carried over as `core/` Rust tests —
that is the parity gate (preserve byte-for-byte: the spectral mask, DEFAULTS/LOOSE
thresholds, the score formula, the spectral glint discriminator, the cluster `id`
hash, presence==done resumability, the parquet hive layout).

## Architecture

```
Cargo.toml          workspace (core + cli + wasm)
core/               PURE compute, no I/O — compiles to wasm and links into the cli.
  src/detect.rs       block detector + tunable Thresholds (defaults / loose),
                      screen_clouds, connected components, enumerate_blocks
  src/cluster.rs      cross-date spatial clustering (Cluster/DedupedDet) + the
                      spectral glint discriminator + deterministic cluster id hash
  src/score.rs        vision-validated cluster quality score + glint geometry
  src/coverage.rs     scl clear-sky sampling (the n_clear_obs persistence denominator)
  src/geo.rs          utm/wgs84, epsg_from_mgrs, bbox helpers
  src/lib.rs          public surface; serde is a feature (off → core is dep-free)
cli/                native gdal-backed binary — reproduces the old cli.js + cf-run.js
  src/read.rs         gdal i/o: /vsis3 eodata JP2 (JP2OpenJPEG, N0400 harmonisation)
                      AND /vsicurl COG byte-ranges — one reader for both paths
  src/stac.rs         STAC search (aws element84 / cdse copernicus profiles), ureq
  src/view.rs         the derived cluster view: duckdb reads the archive / writes the
                      nested-array parquet; rust clusters; csv handoff between them
  src/main.rs         arg parse, aoi loading, rayon fan-out, `detect` / `cluster`
wasm/               wasm-bindgen shim: detectBlock / cluster / scoreCluster → js
cloudferro/         EU-sovereign bulk pipeline (box.sh + cloud-init.yaml)
aoi/                site catalogues that drive runs (raw source + a DuckDB .sql that
                    fits it to the standard AOI geojson schema; see aoi/README.md)
```

The native-vs-wasm seam is the old `detect.js`-takes-typed-arrays / I/O-in-`cog.js`
boundary: `core/` is the pure "detect.js" half (slices in, detections out); I/O
lives in the `cli/` (GDAL) and `wasm/` (JS glue) shells.

## The data model — detections are the archive, clusters are a derived view

This is the central design decision; everything else follows from it.

- **The archive stores DETECTIONS, never clusters.** One row per detection, the
  detector's own field names, hive-partitioned parquet
  `flares/preset=…/mgrs=…/date=…/data.parquet`, written per scene (presence == done
  → resumable, idempotent, incremental). It is **AOI-agnostic**: a flare at
  `(lon,lat)` on a date is a fact independent of the viewport that surfaced it, so
  AOI identity is a query-time tag, not a stored column.
- **Clusters are a derived VIEW.** Clustering is a pure function of `(detections,
  viewport, date-range, thresholds)` — it would go stale the moment a date slider
  moves — so it is run on read, never stored as the archive. Two consumers, one
  `core::cluster_detections`: the CLI (journalist GeoJSON) and the web map (in wasm,
  over raw detections it pulls off the archive).
- **Why not pre-cluster the archive?** Clusters are cross-date aggregates, so they
  can't be written per-scene/idempotently; they bake in a date window; and they'd
  duplicate the detection rows. So the archive stays detections; the view is a
  separate, regenerable artifact (a different bucket prefix, `clusters/…`), computed
  by a separate `s2-flares cluster` run.
- **The view's shape.** One row per cluster + a nested `detections: list<struct>`
  column. A reader column-projects the scalar fields for cheap map pins and only
  fetches the array for drill-down (a double filter for transfer: clustering drops
  the LOOSE tail of rows; column projection drops the array bytes). The CLI's
  journalist GeoJSON and the web-map rollup are the same file shape.

## Key design decisions

- **detect takes typed slices, not images.** I/O is in the shells; detection is pure
  computation — so it runs identically in the native binary and in wasm.
- **Thresholds are a parameter, not constants.** `Thresholds::defaults()` reproduces
  the proven legacy constants exactly; `Thresholds::loose()` keeps the spectral mask
  (the physics) and neutralises the morphological gates for recall-first bulk
  collection (quality gating happens downstream).
- **The spectral mask always runs; the morphological gates are the tunable part.**
  B12/B11 SWIR-hot + background contrast + NHI-SWIR/saturation is what makes this
  flare detection, not bright-pixel detection.
- **cluster_detections is a pure function** with no global state; callers pass a
  cloud-free observation count for the persistence denominator, or `None` to skip.
- **Each cluster has a deterministic `id`** (base36 hash of anchor lat/lon at 4 dp),
  byte-for-byte identical to the JS hash, for stable deep-linking and caching.
- **serde is a `core` feature**, off by default so the pure core stays
  dependency-free; the cli/wasm shells enable it to (de)serialize across their seams.

## Scoring (core/src/score.rs)

`total_score = 0.50·ratio_score + 0.40·persistence_score·(0.1 + 0.9·ratio_score)
− 0.40·min_glint_score`, range −0.40 … +0.90. Vision-validated in permian-flaring
(sql/30_score.sql) on an unbiased aerial study: the B12/B11 ratio is the strongest
precision signal (smooth ramp 1.1→1.7); peak-B12 brightness is a recall floor, not
a ranking term, so it is dropped; clear-sky persistence is ratio-weighted; glint is
the cluster MINIMUM look.

`cluster_detections` also attaches a complementary SPECTRAL glint discriminator
(`median_b12_b11_ratio` / `likely_glint`): a robust median-ratio test (< 1.25 ⇒
glint). The score's geometric `min_glint` (from sun elevation) and this spectral
test measure glint two different ways — both are kept, neither replaces the other.

## CLI (cli/)

Two subcommands over one area (`--bbox W,S,E,N`) or many (`--aoi file.geojson`):

- **`detect`** grows the detection archive: search → concurrent gdal detection
  (rayon) → one CSV per scene under `<out>/<id>/<mgrs>_<date>.csv`, file presence ==
  scene done → resumable. `--source aws` (Element84 COGs, default, no offset) is for
  local testing; `--source cdse` (eodata JP2, harmonised) is the box.
- **`cluster`** derives the view: `core::cluster_detections` over the archive
  (`--archive <duckdb glob>`) or a fresh `detect`, written by `--out` extension —
  `.geojson` (journalist) or nested-array parquet / `s3://…/clusters/…` (web map);
  omit `--out` for GeoJSON to stdout. DuckDB does the parquet/S3 read+write; Rust
  does the clustering; a flat-CSV handoff bridges them (no native parquet deps).

`--preset loose|default` selects thresholds. AOIs are a plain geojson
FeatureCollection; per-dataset schema-fitting lives in a small DuckDB `.sql` in
`aoi/`, not in the tool.

## WebAssembly (wasm/)

`wasm-bindgen` exposes the core to JS: `detectBlock` (typed arrays + a BlockMeta-
shaped object → detections), `cluster` (detection objects → scored sites; accepts
partial objects and the archive's `max_b11` column name via serde), `scoreCluster`.
I/O stays JS glue; only the compute crosses. Built with `wasm-pack`. The web map
clusters raw detections from the archive with the SAME code the CLI runs — no second
clustering implementation to drift.

## CloudFerro (EU-sovereign bulk path)

The same `core`, off US infrastructure: bulk detection on a CloudFerro WAW3-2 box
co-located with the Copernicus `eodata` archive, reading Sentinel-2 `.jp2` directly.

- **`cloudferro/box.sh`** is the whole pipeline, one script: `up` (provision) →
  `run <detect args>` (detached, resumable; rebuilds the binary then runs
  `s2-flares detect --source cdse …` with live progress) → `archive` (grow the
  per-tile parquet collection) → `pull` (rsync CSVs local) → `down` (scale to zero);
  `all` chains them, `ssh`/`ip`/`watch` re-attach.
- **`cloud-init.yaml`** installs rust + gdal + clang (gdal-sys bindgen) + duckdb,
  clones, and `cargo build --release -p s2-flares-cli` at boot (no node).
- **eodata access is per-VM, not anonymous.** The box pulls its own S3 key/secret +
  endpoint from the metadata service at boot into `/etc/profile.d/eodata.sh`; the
  detect binary reads `/vsis3/eodata` via gdal with those env creds.
- **Auth** is OIDC-federated (Keycloak) + 2FA via the vendored official 2FA openrc
  (`s2-flares-openrc-2fa.sh`); box.sh sources it for an authenticated openstack
  session, non-interactive when a gitignored `.env` sets `CLOUDFERRO_PASSWORD` +
  `CLOUDFERRO_TOTP_SECRET` (single-quote the values). The openrc session token lasts
  hours; the TOTP code is spent once per invocation.
- **`archive`** grows a per-tile parquet collection on CloudFerro object storage
  (`s3://$BUCKET/flares/preset=…/mgrs=…/date=…/data.parquet`), one deterministic-key
  parquet per scene (idempotent PUT), queryable in one `read_parquet('s3://…/**/
  *.parquet', hive_partitioning=true)`. DuckDB runs on the box (in-region) with
  project S3 creds from `openstack ec2 credentials`; the box is disposable, the S3
  archive persists.
- **`publish`** makes the archive a web-map backend: anonymous public-read on
  `flares/*` + CORS (via aws-cli — RadosGW S3 ops), so DuckDB-wasm reads the parquet
  directly over HTTP range requests; the hive layout maps viewport tiles+dates
  straight to object URLs (no LIST). Warsaw-only, no CDN — egress ~€0.0064/GB.

## Consumers

Browser consumers (burnoff, gaslight) and the web map use the `wasm/` build —
detection and clustering with the same core. The web map reads raw detections off
the published archive and clusters them in wasm; bulk research runs the CLI on the
box. permian-flaring scored the methodology in DuckDB (sql/30) using the same model
as `core/src/score.rs`.
