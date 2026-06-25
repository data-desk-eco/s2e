# s2-flares

The canonical **Sentinel-2 SWIR gas-flare detection** methodology core: one
minimal ES-module package that detects flares at 20 m from Sentinel-2 L2A
Band 12/11/8A Cloud-Optimized GeoTIFFs (Element84 STAC), scores their quality,
and runs unchanged in the browser, Node/Bun/Deno, a Web Worker, and AWS Lambda.

It is consumed as a git submodule by:

- **burnoff** — client-side P2P detection (browser + Web Worker)
- **gaslight** — single-flare "Enhance" (browser)
- **permian-flaring** — large-area bulk collection (Lambda fan-out → S3 → DuckDB),
  where the quality score was tuned

Pure ES modules, no build step. Browser: zero npm deps (vendored geotiff.js).
Node/Bun/Deno + Lambda: the npm `geotiff` package. The split is hidden behind
`lib/vendor/geotiff-esm.js`.

## What it produces

Each **detection** carries the full discriminating metric set so any gate can be
reconstructed downstream: `max_b12, avg_b12, peak_b11, b12_b11_ratio, peakedness,
pixels, warm_size, saturated, sun_elevation, sun_azimuth, glint_angle,
glint_score`.

`clusterDetections` groups detections across dates into persistent sites and
attaches a single **vision-validated quality score** (`lib/score.js`):

```
total_score = 0.50·ratio_score
            + 0.40·persistence_score·(0.1 + 0.9·ratio_score)
            − 0.40·min_glint_score          range −0.40 … +0.90
```

- `ratio_score` — smooth ramp on the B12/B11 ratio (1.1→1.7); the strongest
  precision signal (brightness is the recall floor, not a ranking term)
- `persistence_score` — the clear-sky share lit (`n_dates / n_clear_obs`)
- `min_glint` — the cluster's minimum geometric glint score (near-nadir specular)

A complementary **spectral** glint flag (`median_b12_b11_ratio` / `likely_glint`)
is also attached. The score is display-only behind an `optional scoreThreshold`.

## Usage

```js
// Streaming async generator (main thread or worker)
import { detect } from './lib/index.js';
for await (const ev of detect(bbox, start, end, { thresholds /* LOOSE for recall */ })) {
    // ev.type: 'image-start' | 'detections' | 'image-done' | 'progress'
}

// Lower-level: search → detect → cluster (+score)
import { searchSTAC } from './lib/stac.js';
import { detectImage } from './lib/cog.js';
import { clusterDetections } from './lib/cluster.js';

const dets = [], obs = new Map();
for await (const item of searchSTAC(bbox, start, end)) {
    const r = await detectImage(item, bbox);          // pass { thresholds } to tune
    obs.set(item.date, { cloudFree: r.cloudFree });
    dets.push(...r.detections);
}
const clusters = clusterDetections(dets, { minDates: 4, minAvgB12: 0.85, observations: obs });
```

Detection thresholds are a parameter, not constants: `detectBlock(...,T)` and
`detectImage(item, bbox, { thresholds })` take a resolved object;
`resolveThresholds(overrides)` merges over `DEFAULTS`. Omitting it reproduces the
proven `DEFAULTS` exactly. `LOOSE` keeps the spectral mask and neutralises the
morphological gates for recall-first bulk collection (filter downstream).

## CLI

Local in-process detection over a bbox (Bun). For large-area bulk runs, deploy
the Lambda and fan scenes out with an S3-writing collector (see permian-flaring).

```bash
bun install
bun cli.js --bbox 51.44,25.84,51.62,25.98 --start 2025-01-01 --end 2025-03-01 --out out.csv
bun cli.js --bbox -104,31.5,-103,32.5 --preset loose --cloud 50 --out permian.csv
bun cli.js --help
```

## Lambda

One invocation = one scene, co-located with the public `sentinel-2-l2a` COGs in
us-west-2 so byte-range reads are in-region. Writes a per-scene CSV to
`s3://$S2_BUCKET/$prefix/<mgrs>_<date>.csv` and returns `{ key, count }` (LOOSE
tiles exceed the 6 MB invoke cap; PutObject is atomic ⇒ resumable). Two modes:
detection (default) and `mode:'coverage'` (SCL clear-sky sampling for the
persistence denominator).

```bash
bash lambda/deploy.sh   # creates IAM role + S3 bucket + function (us-west-2, arm64)
```

All names are env-overridable (`FUNCTION_NAME`, `S3_BUCKET`, `ROLE_NAME`,
`MEMORY`, …) so a consumer deploys the same code under its own names.

## Structure

```
cli.js              Local CLI entry point (Bun)
lib/
  index.js          Public API barrel + detect() async generator
  detect.js         Pure block detector + tunable thresholds (DEFAULTS / LOOSE)
  cog.js            COG I/O (openCOG, readWindow, enumerateBlocks, detectImage)
  coverage.js       SCL clear-sky sampling — the n_clear_obs persistence denominator
  cluster.js        Cross-date spatial clustering (attaches the quality score)
  score.js          Vision-validated cluster quality score + glint geometry
  stac.js           STAC search (Element84, sun angles for glint)
  geo.js            UTM/WGS84 conversions
  worker.js         Web Worker wrapper
  vendor/           Vendored geotiff.js (browser) + environment-aware ESM wrapper
lambda/
  handler.js        detect | coverage mode, writes per-scene CSV to S3
  deploy.sh         One-command deploy (function + IAM role + S3 bucket)
test/               node --test (glint + score); glint-real hits live S2
```

## Tests

```bash
node --test test/glint.test.mjs test/score.test.mjs   # offline
bun test/glint-real.test.mjs                           # live S2 (network)
```
