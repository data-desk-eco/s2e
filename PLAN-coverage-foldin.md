# fold the clear-sky persistence denominator into the detection pass

a handoff plan for a fresh agent. (the other `PLAN.md` is the unrelated gpu
bulk-tile handoff — leave it.) goal: stop running the clear-sky coverage scan as a
separate second read pass and instead measure it **during detection**, so it rides
the fleet's parallelism and SCL is touched once. this is a perf + architecture change
to the **derived view's denominator only** — the score formula and the data model
(detections are the archive, clusters/coverage are derived) do not change.

## why

`persistence = n_dates / n_clear_obs` is the SOTA term (permian-flaring, `sql/30`).
`n_clear_obs` (the clear-sky observation count at a site — the honest denominator,
including the clear-but-UNLIT looks the detection archive can't supply) is today
measured by a **whole separate pass**:

- `cli/src/main.rs::coverage_rescore` (≈l.356): after clustering, per-tile STAC
  search over *every* acquisition (`--cloud 100`), then for each scene
  `read::cover_scene` (`cli/src/read.rs` ≈l.393) does **one whole 20 m SCL band
  read** and samples a 5×5 window at every cluster anchor (`core::cover_sites`),
  classifying `cloud_frac = (shadow+cloud_med+cloud_high+cirrus)/valid`. resumable
  per-scene CSV (`<dir>/<mgrs>_<date>.csv` = `id,cloud_frac`), aggregated to clear
  DATES per site (`cloud_frac ≤ 0.10`), then `Cluster::set_observations(n_clear_obs)`
  rescores via the same `score_cluster`.

this re-searches the same tiles and re-reads SCL for the same acquisitions the
detection pass already swept — a redundant second read pass, and box.sh runs it on
the **head box only** (members 1..N sit idle). on a 26-terminal run it is the long
pole; detection itself is ~instant by comparison once parallel.

the detection pass already opens each scene and (for the point-AOI `GdalReader`)
reads SCL windows when a block is hot. it does NOT currently read SCL for clear-but-
unlit scenes (that is the scan's whole reason to exist) — so the fold-in's real cost
is **one small SCL window read per scene at the AOI site**, added to detection but
replacing the entire second pass, and sharded across the fleet for free.

## target architecture

measure the denominator at detect time, store it per scene next to the detections,
roll it up in `archive`, and have the cluster step read it instead of scanning.

keep it **AOI-agnostic and location-keyed** (the data model): a clear-sky look at
`(lon,lat)` on a date is a fact independent of viewport — store `lon,lat,cloud_frac`
per scene per site, key the rollup by location, and match each cluster anchor to its
nearest site at read time. no AOI identity stored, no AOI list needed at cluster time.

### 1. core (`core/src/coverage.rs`, `core/src/score.rs`) — minimal

- reuse `cover_sites` as-is (it already samples a 5×5 SCL window at arbitrary sites
  and returns the class histogram).
- move the `cloud_frac` derivation (currently inline in `cli/read::cover_scene`,
  `read.rs` ≈l.398: `hist[3]+hist[8]+hist[9]+hist[10]` over `px_valid`) into a small
  `core` helper (e.g. `CoverRow::cloud_frac()`), so detect and cluster share ONE
  classifier and can't drift. keep `CLEAR_MAX = 0.10` as the clear rule.
- `set_observations` / `score_cluster` are UNCHANGED. the fold-in only feeds a
  differently-sourced `n_clear_obs`.

### 2. detection writes a per-scene coverage sidecar (`cli/src/read.rs`, `main.rs`)

- the site for a scene is the **AOI feature's representative point** (centre of the
  AOI det window — `det_bbox(aoi,item)` midpoint; the AOI is a single terminal so one
  site/feature is right). thread the AOI `lon,lat` into the per-scene call.
- in `GdalReader::candidates` (or a thin wrapper in `detect_scene`), after `utm_bbox`
  is known, **always** open SCL (`item.bands.scl`) and sample the site window via
  `cover_sites` → one `cloud_frac` (NaN/absent when no SCL band or site off-tile).
  this is an extra windowed SCL read per scene — cheap (5×5 px decodes one JP2 tile),
  and the only added I/O.
- `run_detect` (`main.rs` ≈l.268): in the success branch, write a sidecar
  `<out>/<aoi.id>/<mgrs>_<date>.cov` = `lon,lat,date,cloud_frac` (one row; the success
  `.csv` is still written for detections, header-only when flareless). presence of the
  `.csv` already means "scene done", so the `.cov` rides the same resumable lifecycle.
  do NOT add columns to the detections `.csv` (keep the parquet schema / parity gate
  intact — see CLAUDE.md).
- note: `detect_scene`'s last arg is `screen_overview`; `run_detect` passes `false`,
  so the existing `cf` bool is meaningless there today. don't rely on it — read SCL at
  the site explicitly.

### 3. `archive` rolls up the sidecars (`cloud/box.sh::archive` + `ARCHIVER`)

- after the gather (already brings every member's CSVs to the head), add a duckdb
  COPY over `out/*/*.cov` → `s3://$BUCKET/coverage/persistence.parquet`, one row per
  site: `lon, lat, n_clear_obs = count(DISTINCT date) FILTER (cloud_frac <= 0.10)`,
  `n_obs = count(DISTINCT date)`. (this is alongside the existing `coverage/` scan
  FOOTPRINT — name them distinctly, e.g. footprint vs persistence.)
- drop the `cluster --coverage-scan` invocation once the new path lands (keep it
  behind a flag during validation, step 5).

### 4. cluster step reads the denominator instead of scanning (`cli/src/main.rs`)

- new path in `run_cluster`: load the per-site persistence rows (a new
  `--coverage <glob|parquet>` arg, read like the detections archive via
  `view::read_*`). after `cluster_detections`, for each cluster find the nearest site
  within an AOI-scale tolerance (~a few km — sites are one-per-terminal) and call
  `set_observations(n_clear_obs)`. unmatched cluster → leave `observations = None`
  (persistence skipped, as today when coverage is absent).
- retire `coverage_rescore`'s STAC-search + `cover_scene` pass (keep the fn behind the
  old `--coverage-scan` flag for validation/fallback only).

### 5. validation (do this BEFORE making it default)

this session's run is the ground truth: `s3://s2-flares-archive/clusters/data.parquet`
(per-cluster `persistence`, anchor-sampled) + `out/coverage/*.csv` (per-anchor
`cloud_frac`). compare fold-in vs scan:

- per cluster: `n_clear_obs` (fold-in, AOI-site) vs scan (anchor), and the resulting
  `persistence_score` / `total_score`.
- acceptance: |Δtotal_score| small (target < ~0.02) for the overwhelming majority and
  **no rank inversions in the top sites**. the only expected difference is the sample
  point (AOI centre vs post-hoc anchor) — for compact terminals they coincide; flag
  any site where they don't and inspect.
- add a `core` unit test for the new classifier helper; keep `cover_sites` tests.

## methodology guardrail

the frozen methodology must not drift (CLAUDE.md): `score_cluster` / the score formula
/ the `0.10` clear rule / the 5×5 window all stay identical. the ONLY change is that
`n_clear_obs` is sampled at the AOI site during detection rather than at the cluster
anchor during a second pass. validate (step 5) that this is score-neutral, document
the change, then make it the default and delete the second pass.

## bonus (note, don't block on it)

storing per-scene clear-sky looks in the archive (location-keyed) means the **web map
can compute `n_clear_obs` live in wasm** for any viewport/date window — today it can
only use the stored full-window snapshot. the fold-in unlocks live persistence on the
map for free.

## files

- `cli/src/main.rs` — `run_detect` (≈248), `run_cluster` (≈300), `coverage_rescore` (≈356)
- `cli/src/read.rs` — `GdalReader::candidates` (≈266), `detect_scene`, `cover_scene` (≈393), `open` (≈108)
- `core/src/coverage.rs` — `cover_sites`, `CoverRow`
- `core/src/cluster.rs` — `Cluster::set_observations` (≈99)
- `core/src/score.rs` — `score_cluster` / `persistence_score` (UNCHANGED)
- `cloud/box.sh` — `archive` (gather + `ARCHIVER` rollup + cluster step)
