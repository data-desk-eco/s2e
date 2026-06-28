# fold the clear-sky denominator into detection — as a spatial cloud mask

a handoff plan for a fresh agent. (the other `PLAN.md` is the unrelated gpu
bulk-tile handoff — leave it.) goal: stop running the clear-sky coverage scan as a
separate second read pass. instead, **during detection** emit a gridded **cloud mask**
(`clouds/`: `(cell, date) → cloud_frac`, AOI-agnostic) and compute the persistence
denominator by **spatial-joining each cluster anchor against it** — so SCL is touched
once, the work rides the fleet, and AOIs leave the stored data model entirely (they
are only the scan extent). the score formula is UNCHANGED; only the source of
`n_clear_obs` moves (a second SCL pass → a join against the mask).

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
is **one SCL read per scene, gridded over the scan window into a cloud mask**, added
to detection but replacing the entire second pass, and sharded across the fleet for
free.

## target architecture — a cloud mask, spatial-joined (no AOIs in the data)

persistence is a SPATIAL question — "how many clear looks at THIS point?" — so the
right artifact is a **gridded cloud layer per scene** that you spatial-join against the
detections at cluster time. AOIs disappear from the stored model entirely: they are
only the scan EXTENT (where the detector looked), never a key or a column — exactly
like detections already are (a fact at `(lon,lat,date)`, AOI-agnostic). this is cleaner
than per-AOI/per-site coverage and it's reusable: a cloud mask is useful to anything
spatial, not tied to this run's terminals.

the archive is being repopulated from scratch — no back-compat with `coverage/`. one
detect pass produces:
- `detections/` — one row per hot pixel (`lon,lat,date,b12,b11,…`). UNCHANGED, pure
  (parity gate), AOI-agnostic.
- `clouds/` — the cloud mask: one row per **(cell, date)** = `glon, glat, date,
  cloud_frac`, where `glon/glat` are `lon/lat` snapped to a fixed grid (~100 m). a
  per-scene slice of a global mask; overlapping scans of a cell on a date dedup
  (`DISTINCT`, like detections). REPLACES `coverage/` AND the per-AOI `observations/`
  idea outright. the scan FOOTPRINT (Detect-button gating) derives from it too (which
  cells/dates were observed).
- `clusters/` — derived; `n_clear_obs` comes from a SPATIAL JOIN of each anchor's cell
  against `clouds/`.

**why a grid, not per-AOI rows:** a clear look is a fact about a place on a date, not
about an AOI. keying coverage by AOI re-imports viewport identity into the archive
(overlapping AOIs double-count; the web map can't reuse it for arbitrary viewports).
the grid is the AOI-agnostic shape — and the denominator falls straight out of the
spatial join you'd reach for anyway: `anchor_cell ⋈ clouds(cell,date)`.

**why not just a column on detection rows:** the denominator is made of clear-but-UNLIT
cells — observed clear with NO flare → zero detection rows. so cloud state must live in
its own per-(cell,date) layer covering the whole scanned window, not only where pixels
lit.

### 1. core — grid the SCL window into cells (`core/src/coverage.rs`, `core/src/score.rs`)

- reuse `cover_sites` (it already samples a 5×5 window at arbitrary sites): generate a
  grid of cell-centre sites tiling the scan window at the grid step, pass them in →
  `cloud_frac` per cell. add a thin `grid_sites(bbox, step)` helper (or have the caller
  build the grid).
- move the `cloud_frac` derivation (inline in `cli/read::cover_scene`, `read.rs`
  ≈l.398: `(hist[3]+hist[8]+hist[9]+hist[10]) / px_valid`) into a shared `core` helper
  (e.g. `CoverRow::cloud_frac()`) so detect + cluster share ONE classifier. keep
  `CLEAR_MAX = 0.10`.
- `set_observations` / `score_cluster` UNCHANGED — only the source of `n_clear_obs`
  changes (now: count of distinct clear dates for the anchor's cell).

### 2. detection emits the cloud-mask slice (`cli/src/read.rs`, `main.rs`)

- in `GdalReader::candidates` (or a wrapper in `detect_scene`), after `utm_bbox` is
  known, **always** open SCL (`item.bands.scl`) and sample it over the **grid covering
  the scan window** → `(glon, glat, cloud_frac)` per cell. this is the only added I/O —
  one SCL read per scene (incl. flareless/cloudy: that is the unlit denominator). today
  `GdalReader` reads SCL only lazily at hot blocks, so this is genuinely new per-scene
  work, but it's cheap (a 20 m band) and rides the fleet.
- `run_detect` (`main.rs` ≈l.268): write a sibling per-scene file
  `<out>/<aoi.id>/<mgrs>_<date>.cld` = rows `glon,glat,date,cloud_frac`, in BOTH the
  success and flareless branches (the detections `.csv`/`.err` still drives
  resumability; the `.cld` rides it). do NOT touch the detections `.csv` schema (parity
  gate).
- snap `glon/glat` to the grid (round `lon/lat` to ~3 dp ≈ 100 m) — the SAME snapping
  used at join time (step 4). note: `detect_scene`'s `screen_overview` arg is `false`
  in `run_detect`, so the existing `cf` bool is meaningless there — read SCL explicitly.

### 3. `archive` rolls the cloud mask up — `coverage/` → `clouds/` (`cloud/box.sh::archive` + `ARCHIVER`)

- after the gather, duckdb COPY over `out/*/*.cld` → `s3://$BUCKET/clouds/data.parquet`,
  `SELECT DISTINCT glon,glat,date,cloud_frac … ORDER BY date` (dedup overlapping scans,
  date row-group pruning). hive-partition by `mgrs` if it helps range reads.
- this REPLACES `coverage/` — both downstream uses derive from `clouds/`:
  - persistence: per cell, `n_clear_obs = count(DISTINCT date) FILTER (cloud_frac ≤ 0.10)`.
  - scan FOOTPRINT: which cells/tiles/dates were observed (group by cell/tile).
- DELETE the `cluster --coverage-scan` invocation and the standalone footprint COPY in
  `ARCHIVER` (keep `coverage_rescore` behind its flag for validation only, step 5).

### 4. clustering spatial-joins the mask (`cli/src/main.rs`)

- new `--clouds <glob|parquet>` arg; `run_cluster` loads `clouds/` via `view::read_*`.
- after `cluster_detections`, for each cluster snap its anchor `(lon,lat)` to the grid →
  cell, and `n_clear_obs = count of DISTINCT dates where clouds[cell].cloud_frac ≤ 0.10`
  → `set_observations(n_clear_obs)`. this is the spatial join (a hash join on snapped
  coords; widen to the 8 neighbouring cells if an anchor lands on a cell edge). a
  cluster whose cell has no cloud rows → `observations = None` (persistence skipped, as
  today when coverage is absent).
- retire `coverage_rescore`'s STAC-search + `cover_scene` pass (keep behind the old
  `--coverage-scan` flag for validation/fallback only).

### grid resolution

snap to ~100 m (3 dp) to match the original 5×5 @ 20 m sampling window — the anchor's
cell ≈ the old per-anchor window, so the denominator is near-identical. tunable; coarsen
(fewer rows) only with the step-5 validation re-run. the ONLY hard rule: detection and
clustering must snap to the SAME grid or the join misses.

### 5. validation (do this BEFORE making it default)

this session's run is the ground truth: `s3://s2-flares-archive/clusters/data.parquet`
(per-cluster `persistence`, anchor-sampled) + `out/coverage/*.csv` (per-anchor
`cloud_frac`). compare fold-in vs scan:

- per cluster: `n_clear_obs` (cloud mask — the anchor's grid cell) vs scan (anchor's
  5×5 window), and the resulting `persistence_score` / `total_score`.
- acceptance: |Δtotal_score| small (target < ~0.02) for the overwhelming majority and
  **no rank inversions in the top sites**. both sample at the anchor location (~100 m),
  so they should be near-identical; the only difference is grid snapping vs an exact
  centre — flag any site where they diverge and inspect (likely a cell-edge anchor →
  the 8-neighbour widen in step 4).
- add a `core` unit test for the grid helper + the shared classifier; keep `cover_sites`
  tests.

## methodology guardrail

the frozen methodology must not drift (CLAUDE.md): `score_cluster` / the score formula
/ the `0.10` clear rule / the 5×5 (≈100 m) window all stay identical. the ONLY change
is that the clear look is sampled onto a ~100 m grid during detection and the anchor's
cell is read back via spatial join, rather than re-sampled at the anchor in a second
pass — same location, same window scale. validate (step 5) that this is score-neutral,
document it, then make it default and delete the second pass.

## bonus (note, don't block on it)

- a location-keyed cloud mask means the **web map can compute `n_clear_obs` live in
  wasm** for any viewport/date window (spatial-join detections against the mask in the
  browser) — today it can only use the stored full-window snapshot. live persistence on
  the map, for free.
- it also **fixes a window-matching bug for free**: the separate scan was driven by
  `box.sh archive`'s `START/END` (default `2015-01-01 → 2100-01-01`), so persistence
  counted a decade of clear looks against an 18-month detection window — a deflated
  denominator (and a ~7× slower scan). tying observations to the scenes detection
  actually swept makes the persistence window ALWAYS equal the detection window.

## files

- `cli/src/main.rs` — `run_detect` (≈248), `run_cluster` (≈300), `coverage_rescore` (≈356)
- `cli/src/read.rs` — `GdalReader::candidates` (≈266), `detect_scene`, `cover_scene` (≈393), `open` (≈108)
- `core/src/coverage.rs` — `cover_sites`, `CoverRow`
- `core/src/cluster.rs` — `Cluster::set_observations` (≈99)
- `core/src/score.rs` — `score_cluster` / `persistence_score` (UNCHANGED)
- `cloud/box.sh` — `archive` (gather + `ARCHIVER` rollup + cluster step)
