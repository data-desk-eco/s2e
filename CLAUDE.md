# s2-flares

The canonical Sentinel-2 SWIR flare-detection methodology core — one minimal,
elegant ES-module package that runs in the browser, Node/Bun/Deno, a Web Worker,
and AWS Lambda. It supersedes the openflaring lineage: detector, glint geometry,
the vision-validated quality score, and the bulk Lambda collector all live here.

Consumers (via git submodule):
- **burnoff** — client-side P2P detection (browser + Web Worker).
- **gaslight** — single-flare "Enhance" (browser, relaxed thresholds).
- **permian-flaring** — large-area bulk collection (Lambda fan-out → S3 → DuckDB);
  also the research notebook where the score was tuned.

## Architecture

Pure ES modules. No build step. Workers use `{ type: 'module' }`. Browser
consumers have zero npm dependencies (vendored geotiff.js UMD); Node/Bun/Deno and
Lambda use the npm `geotiff` package. The split is hidden behind
`lib/vendor/geotiff-esm.js`, which loads the right one per environment.

```
cli.js              CLI entry point (Bun): --bbox or --aoi geojson; local or --lambda fan-out
lib/
  index.js          Public API barrel + detect() async generator
  run.js            Whole-AOI pipeline (search → concurrent detect → cluster);
                    shared by the CLI local mode and the Lambda web API
  detect.js         Pure block detector + tunable thresholds (DEFAULTS / LOOSE)
  cog.js            COG I/O (openCOG, readWindow, enumerateBlocks, detectImage)
  coverage.js       SCL clear-sky sampling — the n_clear_obs persistence denominator
  cluster.js        Cross-date spatial clustering (pure function, attaches score)
  score.js          Vision-validated cluster quality score + glint geometry
  stac.js           STAC search (Element84, async generator, sun angles for glint)
  geo.js            UTM/WGS84 conversions + degree helpers
  worker.js         Web Worker wrapper (postMessage interface)
  vendor/
    geotiff.js      Vendored geotiff.js 2.1 (UMD, browser only)
    geotiff-esm.js  Environment-aware wrapper (browser: UMD, Node/Bun/Deno: npm)
lambda/
  handler.js        Per-scene bulk handler: detect | coverage mode, per-scene CSV to S3
  api.js            Web API handler: geometry + dates → clustered GeoJSON, over a
                    streaming Function URL (buffered JSON or live NDJSON), area-capped
  deploy.sh         One-command deploy to us-west-2 (function + IAM + S3 bucket;
                    HANDLER=lambda/api.handler PUBLIC_URL=1 deploys the web API)
aoi/                Site catalogues that drive runs (raw source + a DuckDB .sql that
                    fits it to the standard AOI geojson schema; see aoi/README.md)
  lng-terminals.sql / .sh   Global LNG export terminals (GEM) → AOIs → Lambda fan-out
```

## Key Design Decisions

- **detect.js takes typed arrays, not GeoTIFF images.** I/O is in cog.js;
  detection is pure computation. This lets burnoff do its own I/O with caching and
  P2P partitioning.
- **Thresholds are a parameter, not constants.** `detectBlock(..., T)` and
  `detectImage(item, bbox, { thresholds })` take a resolved thresholds object;
  `resolveThresholds(overrides)` merges over `DEFAULTS`. Omitting it reproduces the
  proven DEFAULTS exactly (so burnoff's 5-arg calls are byte-for-byte unchanged).
  `LOOSE` keeps the spectral mask (the physics) and neutralises the morphological
  gates for recall-first bulk collection — quality gating then happens downstream.
- **The spectral mask always runs; the morphological gates are the tunable part.**
  B12/B11 SWIR-hot + background contrast + NHI-SWIR/saturation is what makes this
  flare detection, not bright-pixel detection.
- **clusterDetections is a pure function** with no global state. Consumers pass an
  `observations` map for the persistence denominator, or null to skip.
- **Each cluster has a deterministic `id`** (hash of anchor lat/lon at 4 dp) for
  stable deep linking and caching.

## Scoring (lib/score.js)

`total_score = 0.50·ratio_score + 0.40·persistence_score·(0.1 + 0.9·ratio_score)
− 0.40·min_glint_score`, range −0.40 … +0.90. Vision-validated in permian-flaring
(sql/30_score.sql) on an unbiased aerial study: the B12/B11 ratio is the strongest
precision signal (smooth ramp 1.1→1.7); peak-B12 brightness is a recall floor, not
a ranking term, so it is dropped; clear-sky persistence is ratio-weighted; glint is
the cluster MINIMUM look. permian's three hard gates (far-from-facility,
on-building, on-road) need ground layers and live in its SQL, not here.

`clusterDetections` also attaches a complementary SPECTRAL glint discriminator
(`median_b12_b11_ratio` / `likely_glint`, `glintMetrics` in cluster.js): a robust
median-ratio test (< 1.25 ⇒ glint) proven on gaslight clusters and unit-tested.
The score's geometric `min_glint` (from sun elevation) and this spectral test
measure glint two different ways — both are kept, neither replaces the other.

## CLI

The single entrypoint, over one area (`--bbox W,S,E,N`) or many (`--aoi
file.geojson` — one run per feature, its geometry bounds + `--buffer` km as the
search box, its `id`/`name` tagging the output). `--preset loose|default` selects
thresholds. Shares all code with the browser/Lambda paths.

- **Local (default):** in-process detection + clustering; CSV out (one row per
  detection carrying cluster fields), auto-converted to Parquet if `duckdb` is on PATH.
- **Bulk (`--lambda FN`):** instead of detecting locally, fan each scene out to the
  deployed Lambda, which writes per-scene CSVs to S3 (`--bucket`/`--prefix`, per-AOI
  prefix `<prefix>/<id>/`); resumable, scoring happens downstream. This folds the old
  bespoke collector into the CLI — there is no separate fan-out script.

AOIs are a plain geojson FeatureCollection (features with `id`/`name`). The burden
of fitting a vendor dataset to that schema (filtering, dedup, geometry) lives in a
small DuckDB `.sql` kept beside the data in `aoi/`, not in the tool.

## Lambda

The detector core lives in `lib/`; the Lambda is an I/O shell around it,
co-located with the public `sentinel-2-l2a` COGs in us-west-2 so byte-range reads
are in-region. One invocation = one scene.

- **Writes results to S3, not the invoke response.** Under LOOSE an interior tile
  exceeds the 6 MB synchronous-response cap, so the handler writes a per-scene CSV
  to `s3://$S2_BUCKET/$prefix/<mgrs>_<date>.csv` and returns only `{key, count}`.
  PutObject is atomic, so object presence == scene done → trivially resumable.
- **Two modes:** detection (default) and `mode: 'coverage'` (SCL clear-sky
  sampling at catalogue sites, for the persistence denominator).
- **Event:** `{ item, bbox, thresholds?, screenOverview?, prefix?, mode?, sites?, chunk? }`.
- Runtime nodejs22.x, arm64. AWS SDK v3 is runtime-provided (not bundled).
- Deploy: `bash lambda/deploy.sh` — creates the IAM role, the detections bucket,
  and the function. All names are env-overridable (`FUNCTION_NAME`, `S3_BUCKET`,
  `MEMORY`, …) so a consumer can deploy the same code under its own names.

## Web API (lambda/api.js)

A second handler turns the same `lib/` core into an HTTP endpoint: POST/GET a
geometry (or `bbox`) + date range, get back clustered, scored flare detections as
GeoJSON. It calls `runAOI` (lib/run.js) — the whole-AOI pipeline the CLI also uses
— so search → detect → cluster lives in one place, not duplicated per entry point.

- **Front door is a Lambda Function URL, not API Gateway.** One handler, built-in
  HTTPS + CORS, no 29 s gateway cap. Deployed in `RESPONSE_STREAM` invoke mode so a
  single handler serves both response shapes:
  - default — one JSON `FeatureCollection`, written when detection completes.
  - `?stream=1` (or `Accept: text/event-stream`) — newline-delimited JSON events
    (`start` / `scene` / final `clusters`) as each scene finishes. Built for an
    interactive map: live pins + a progress bar while panning. Clustering is
    cross-date, so the map features arrive only in the terminal `clusters` event.
- **Hard area cap.** `MAX_AOI_KM2` (default 2500) rejects oversized AOIs with 413
  *before any COG read* — a public endpoint that bounds the work per request. The
  natural client is a map viewport, which is small; "zoom in" is the contract.
  Pair it with `MAX_CONCURRENCY` reserved concurrency as a cost ceiling.
- **Map-friendly clustering.** Defaults to `minDates=1` so every scored detection
  surfaces; the client filters by the `total_score` slider. `minScore`/`minDates`
  are request overrides. Bulk-research gating (`minDates=4`) stays the CLI's job.
- **Request:** `{ geometry | bbox, start?, end?, buffer?, preset?, stream?, minScore?, minDates? }`
  (query string for GET, JSON body for POST; body wins). Dates default to the last
  `DEFAULT_DAYS` (90).
- Deploy: `FUNCTION_NAME=s2-flares-api HANDLER=lambda/api.handler PUBLIC_URL=1 bash
  lambda/deploy.sh` — same script, adds the streaming Function URL + public-invoke
  permission + reserved concurrency, and prints the URL.

## Consumers

- **burnoff**: lower-level functions (searchSTAC, openCOG, readWindow,
  enumerateBlocks, detectBlock) for P2P block partitioning and CRDT caching;
  clusterDetections with terminal naming. Runs DEFAULTS.
- **gaslight**: the worker.js message interface for single-flare enhancement.
  Relaxed clustering (minDates=1, minAvgB12=0.5).
- **permian-flaring**: deploys lambda/ for the bulk fan-out, runs LOOSE, scores in
  DuckDB (sql/30) using the same methodology as lib/score.js.
