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
cli.js              Local CLI entry point (Bun)
lib/
  index.js          Public API barrel + detect() async generator
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
  handler.js        Lambda handler: detect | coverage mode, writes per-scene CSV to S3
  deploy.sh         One-command deploy to us-west-2 (function + IAM + S3 bucket)
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

Local in-process detection over a bbox (`bun cli.js --bbox W,S,E,N`). `--preset
loose|default` selects thresholds. Shares all code with the browser/Lambda paths.
Output is CSV (one row per detection, carrying cluster fields), auto-converted to
Parquet if `duckdb` is on PATH. For large-area bulk collection use the Lambda +
an S3-writing fan-out collector (permian-flaring's scripts/collect_s2.mjs).

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

## Consumers

- **burnoff**: lower-level functions (searchSTAC, openCOG, readWindow,
  enumerateBlocks, detectBlock) for P2P block partitioning and CRDT caching;
  clusterDetections with terminal naming. Runs DEFAULTS.
- **gaslight**: the worker.js message interface for single-flare enhancement.
  Relaxed clustering (minDates=1, minAvgB12=0.5).
- **permian-flaring**: deploys lambda/ for the bulk fan-out, runs LOOSE, scores in
  DuckDB (sql/30) using the same methodology as lib/score.js.
