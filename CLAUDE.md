# s2-flares

The canonical Sentinel-2 SWIR flare-detection methodology core, as a minimal Rust
workspace: one pure methodology core that compiles both to a fast native CLI and to
WebAssembly for the browser. It supersedes the openflaring/JS lineage — detector,
glint geometry, the vision-validated quality score, clustering, and the bulk
collector all live here, once, in Rust.

The frozen methodology must not drift. The pure compute is ported 1:1 from the
retired JS `lib/`, and the JS unit suites are carried over as `core/` Rust tests —
that is the parity gate (preserve byte-for-byte: the spectral mask, the score
formula, the spectral glint discriminator, the cluster `id` hash, presence==done
resumability, the parquet hive layout).

## Architecture

```
Cargo.toml          workspace (core + cli + wasm)
core/               PURE compute, no I/O — compiles to wasm and links into the cli.
  src/detect.rs       block detector + tunable Thresholds (recall-first defaults;
                      every gate a parameter), screen_clouds, components, blocks +
                      hot_core (the flare's combustion-hot area + integrated radiance:
                      pixels/radiance, the volume signal — NOT the loose-mask flood)
  src/cluster.rs      cross-date spatial clustering (Cluster/DedupedDet) + the
                      spectral glint discriminator + deterministic cluster id hash
  src/score.rs        vision-validated cluster quality score + glint geometry
  src/coverage.rs     scl clear-sky sampling + the ~100 m cloud-mask grid (grid_sites,
                      cell_key, CoverRow::cloud_frac) — the n_clear_obs denominator,
                      now emitted as a gridded mask during detection, not a 2nd pass
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
cloud/              EU-sovereign bulk pipeline (box.sh + cloud-init.yaml)
aoi/                site catalogues that drive runs (raw source + a DuckDB .sql that
                    fits it to the standard AOI geojson schema; see aoi/README.md)
```

The native-vs-wasm seam is the old `detect.js`-takes-typed-arrays / I/O-in-`cog.js`
boundary: `core/` is the pure "detect.js" half (slices in, detections out); I/O
lives in the `cli/` (GDAL) and `wasm/` (JS glue) shells.

## The data model — detections are the archive, clusters are a derived view

This is the central design decision; everything else follows from it.

- **The archive stores DETECTIONS, never clusters.** One row per detection, the
  detector's own field names. **Resumability is the per-scene CSV layer** — `detect`
  writes one `<mgrs>_<date>.csv` per scene, presence == done → resumable, idempotent,
  incremental. The published parquet is a **per-tile rollup** of those CSVs
  (`detections/mgrs=…/data.parquet`, date a column not a path level): one file per
  MGRS tile, ~10² objects of useful size rather than ~10⁴ tiny per-scene files —
  fewer footer/range reads for both bulk scans and the web map. The parquet
  granularity is free precisely because resumability lives in the CSVs, not here.
  It is **AOI-agnostic**: a flare at `(lon,lat)` on a date is a fact independent of
  the viewport that surfaced it, so AOI identity is a query-time tag, not a stored
  column — and the rollup `SELECT DISTINCT`s across every AOI's CSVs for a tile,
  unioning overlapping-AOI detections rather than letting one clip win.
- **Clusters are a derived VIEW.** Clustering is a pure function of `(detections,
  viewport, date-range, thresholds)` — it would go stale the moment a date slider
  moves — so it is run on read, never stored as the archive. Two consumers, one
  `core::cluster_detections`: the CLI (journalist GeoJSON) and the web map (in wasm,
  over raw detections it pulls off the archive).
- **Why not pre-cluster the archive?** Clusters are cross-date aggregates that bake
  in a date window and would duplicate the detection rows — so the *source of truth*
  stays detections, never clusters. The cluster view is a separate, regenerable
  artifact in its own prefix (`clusters/…`). The box **co-produces it in the rollup
  pass** (`archive` writes both `detections/` and `clusters/data.parquet`) for
  freshness and one fewer command — but it is still derived, not authoritative: the
  web map re-clusters raw detections live in wasm for any viewport/date window the
  stored full-window snapshot doesn't cover.
- **The view's shape.** One row per cluster + a nested `detections: list<struct>`
  column. A reader column-projects the scalar fields for cheap map pins and only
  fetches the array for drill-down (a double filter for transfer: clustering drops
  the LOOSE tail of rows; column projection drops the array bytes). The CLI's
  journalist GeoJSON and the web-map rollup are the same file shape.

## Key design decisions

- **detect takes typed slices, not images.** I/O is in the shells; detection is pure
  computation — so it runs identically in the native binary and in wasm.
- **Thresholds are a parameter, not constants — and not presets.** Every gate is a
  field of `Thresholds`; `Thresholds::default()` is the one sensible baseline:
  recall-first — the full spectral mask runs, the morphological size gates are
  neutralised (quality gating happens downstream at cluster/score). The cli exposes
  each key variable as its own flag (`--b12-min`, `--contrast-ratio`, …) overriding
  that baseline; wasm takes an optional partial thresholds object. There is no
  loose/default preset dial — you tune the variables you care about directly.
- **The spectral mask always runs; the morphological gates are the tunable part.**
  B12/B11 SWIR-hot + background contrast + NHI-SWIR/saturation is what makes this
  flare detection, not bright-pixel detection.
- **Flare SIZE/VOLUME comes from the hot core, never the detection mask.** The loose
  recall-first mask 4-connects a flare's peak across the entire warm facility (a single
  Ras Laffan flare flooded to ~36k px ≈ 15 km²). So `pixels`/`radiance` are measured on
  the flare's own connected component restricted to combustion-hot pixels (B12 above
  `hot_floor`, grown from the peak): `pixels` is the hot-core area, `radiance` is its
  integrated SWIR excess Σ(b12 − background). Volume estimation reads these two plus
  `max_b12`/`saturated` — and because `max_b12` pegs at saturation, `radiance` (area ×
  intensity) is what keeps ranking the biggest flares once their cores clip. The mask's
  flooded component count is discarded; it never had a downstream use but as this number.
- **cluster_detections is a pure function** with no global state; callers pass a
  cloud-free observation count for the persistence denominator, or `None` to skip.
  `Cluster::set_observations(n_clear_obs)` re-attaches a per-site measured denominator
  and rescores via the same `score_cluster` (the archive coverage path == the
  fresh-detect path; one scoring impl, no drift).
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
  scene done → resumable. Alongside each CSV it writes a sibling `.cld` — the per-scene
  **cloud-mask slice** (`glon,glat,date,cloud_frac` over a ~100 m grid covering the scan
  window, via `core::grid_sites` + one whole-band SCL read), emitted for EVERY scene incl.
  flareless/cloudy. That is the clear-sky persistence denominator FOLDED INTO detection —
  no separate second SCL pass. `--source aws` (Element84 COGs, default, no offset) is for
  local testing; `--source cdse` (eodata JP2, harmonised) is the box.
- **`cluster`** derives the view: `core::cluster_detections` over the archive
  (`--archive <duckdb glob>`) or a fresh `detect`, written by `--out` extension —
  `.geojson` (journalist) or nested-array parquet / `s3…/clusters/…` (web map);
  omit `--out` for GeoJSON to stdout. DuckDB does the parquet/S3 read+write; Rust
  does the clustering; a flat-CSV handoff bridges them (no native parquet deps).
  `--clouds <glob>` adds the SOTA persistence (permian-flaring) by a **spatial join**:
  snap each cluster anchor to its ~100 m cell, `n_clear_obs = distinct dates where that
  cell's `cloud_frac ≤ 0.10`` (∪ the site's own detection dates), then
  `Cluster::set_observations` rescores → real `persistence = n_dates / n_clear_obs`.
  Pure DuckDB+Rust, no eodata/gdal, no re-read. (The legacy `--coverage-scan <dir>` —
  re-sampling SCL at every anchor over every acquisition — is kept only to cross-check
  the fold-in; same `core::cover_sites` classifier, so they agree.) The min-dates floor
  stays recall-first (drop true singletons only, `n_dates ≥ 2`); persistence is a
  continuous score term, never a hard count gate — a count gate discards dim-but-persistent flares.

Detector thresholds default recall-first; tighten any single variable with its own
flag (`--b12-min`, `--b11-min`, `--peak-b12-min`, `--contrast-ratio`,
`--background-floor`, `--peakedness-min`). AOIs are a plain geojson FeatureCollection;
per-dataset schema-fitting lives in a small DuckDB `.sql` in `aoi/`, not in the tool.

## WebAssembly (wasm/)

`wasm-bindgen` exposes the core to JS: `detectBlock` (typed arrays + a BlockMeta-
shaped object + an optional partial thresholds object → detections), `cluster`
(detection objects → scored sites; accepts
partial objects and the archive's `max_b11` column name via serde), `scoreCluster`.
I/O stays JS glue; only the compute crosses. Built with `wasm-pack`. The web map
clusters raw detections from the archive with the SAME code the CLI runs — no second
clustering implementation to drift.

## CloudFerro (EU-sovereign bulk path)

The same `core`, off US infrastructure: bulk detection on a CloudFerro WAW3-2 box
co-located with the Copernicus `eodata` archive, reading Sentinel-2 `.jp2` directly.

- **`cloud/box.sh`** is the whole pipeline, one script: `image` (bake the golden disk
  image once, optional) → `up` (provision) →
  `run <detect args>` (detached, resumable; rebuilds the binary then runs
  `s2-flares detect --source cdse …` with live progress) → `archive` (grow the
  per-tile parquet collection) → `pull` (rsync CSVs local) → `down` (scale to zero);
  `all` chains them, `ssh`/`ip`/`watch` re-attach.
- **`image`** bakes a golden snapshot to skip the ~5-8min cold install+build on every
  boot. It boots one stock box, lets cloud-init do the full install+build, strips the
  per-VM creds + cloud-init state, snapshots the disk to `$BASEIMG` (`s2-flares-base`),
  and tears the box down. Thereafter `up` auto-boots from that snapshot (`resolve_image`):
  the SAME `cloud-init.yaml`'s guards no-op against the on-disk toolchain/tree, so a
  member is ready in <1min — only the per-VM eodata creds and `start_member`'s incremental
  `git pull && cargo build` run live. Re-run `image` to refresh the snapshot (e.g. after a
  `Cargo.lock`/system-lib change); the deps, not the source binary, are what's worth baking.
- **`cloud-init.yaml`** installs rust + gdal + clang (gdal-sys bindgen) + duckdb,
  clones, and `cargo build --release -p s2-flares-cli` at boot (no node). EVERY heavy
  step is GUARDED by a presence check, so one file serves both boots: a full cold build
  on the stock distro, and a near-instant no-op when booted from the golden `$BASEIMG`
  (toolchain + tree already on disk). The per-VM eodata creds step is the only ungated
  one — it must rewrite for each box; `image` strips it before snapshotting.
- **eodata access is per-VM, not anonymous.** The box pulls its own S3 key/secret +
  endpoint from the metadata service at boot into `/etc/profile.d/eodata.sh`; the
  detect binary reads `/vsis3/eodata` via gdal with those env creds.
- **Auth** is OIDC-federated (Keycloak) + 2FA via the vendored official 2FA openrc
  (`s2-flares-openrc-2fa.sh`); box.sh sources it for an authenticated openstack
  session, non-interactive when a gitignored `.env` sets `CLOUDFERRO_PASSWORD` +
  `CLOUDFERRO_TOTP_SECRET` (single-quote the values). The openrc session token lasts
  hours; the TOTP code is spent once per invocation.
- **`archive`** rolls the per-scene CSVs+`.cld` up into THREE artifacts in one pass.
  `detections/`: a per-tile parquet collection on CloudFerro object storage
  (`s3://$BUCKET/detections/mgrs=…/data.parquet`), one deterministic-key parquet per
  MGRS tile (idempotent PUT; a `SELECT DISTINCT … ORDER BY date` union of that tile's
  scene CSVs), queryable in one `read_parquet('s3://…/**/*.parquet',
  hive_partitioning=true)`. `clouds/`: the cloud mask, an immutable deterministic
  per-run collection under `clouds/runs/` (plus legacy `clouds/data.parquet`) —
  AOI-agnostic, INTERNAL, and not web-published. Per-run objects avoid a global
  DISTINCT rewrite/scratch disk; the cluster join set-deduplicates clear dates per cell
  across objects. Kept in S3 to re-join another date window without re-reading SCL.
  `clusters/`: the derived view (`clusters/data.parquet`),
  produced by running `s2-flares cluster --clouds` over the fresh `detections/`
  full-window — the persistence denominator is a **spatial join** of each anchor's cell
  against `clouds/` (no second SCL pass). DuckDB (rollup) and the cli (cluster) both run
  on the box in-region; the cluster step is now pure DuckDB on the project bucket via
  `S2_S3_*` env (from `openstack ec2 credentials`) — the clouds join needs no eodata/gdal,
  so no second credential set. The box is disposable, S3 persists.
- **`publish`** makes the archive a web-map backend: anonymous public-read on
  `detections/*` + `clusters/*` + CORS (via aws-cli — RadosGW S3 ops), so DuckDB-wasm
  reads the parquet directly over HTTP range requests — scalar pins from `clusters/`,
  live reclustering from `detections/` (the hive layout maps each viewport tile
  straight to its object URL, no LIST, date filtered in-file via row-group stats).
  `clouds/` is NOT published (the anchor⋈mask join is build-time, baked into `clusters/`;
  the browser reads the baked persistence). Warsaw-only, no CDN — egress ~€0.0064/GB.

## Consumers

Browser consumers (burnoff, gaslight) and the web map use the `wasm/` build —
detection and clustering with the same core. The web map reads raw detections off
the published archive and clusters them in wasm; bulk research runs the CLI on the
box. permian-flaring scored the methodology in DuckDB (sql/30) using the same model
as `core/src/score.rs`.
