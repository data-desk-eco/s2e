# s2-flares

Sentinel-2 SWIR flare detection library. Detects gas flares at 20m resolution using Sentinel-2 L2A Band 12/11/8A imagery via Cloud Optimized GeoTIFFs from the Element84 STAC catalog.

Pure ES modules. Browser: zero npm dependencies (vendored geotiff.js). CLI/Lambda: uses npm `geotiff` package for Node.js/Bun compatibility.

## Usage

### Async generator (main thread or worker)

```js
import { detect } from './lib/index.js';

for await (const event of detect(bbox, startDate, endDate, { signal })) {
    // event.type: 'image-start' | 'detections' | 'image-done' | 'progress'
}
```

### Web Worker

```js
const w = new Worker('lib/worker.js', { type: 'module' });
w.postMessage({ type: 'detect', bbox, start, end, clusterOptions: { minDates: 1 } });
w.onmessage = (e) => { /* detections | progress | clusters | error | done */ };
```

### Lower-level API

```js
import { searchSTAC } from './lib/stac.js';
import { detectImage } from './lib/cog.js';
import { clusterDetections } from './lib/cluster.js';

const items = [];
for await (const item of searchSTAC(bbox, start, end)) items.push(item);

const allDetections = [];
for (const item of items) {
    const { detections } = await detectImage(item, bbox);
    allDetections.push(...detections);
}

const clusters = clusterDetections(allDetections, { minDates: 4, minAvgB12: 0.85 });
```

## CLI

Bulk detection over large bounding boxes, with optional AWS Lambda fan-out for scale.

```bash
bun install

# Small area — local mode
bun cli.js --bbox 51.44,25.84,51.62,25.98 --start 2025-01-01 --end 2025-03-01

# Permian Basin — Lambda mode (scenes processed in us-west-2, co-located with S2 COGs)
bun cli.js --bbox -104.0,31.5,-103.0,32.5 --start 2025-01-01 --end 2025-02-01 --mode lambda --concurrency 8

# Options
bun cli.js --help
```

Deploy the Lambda (requires AWS CLI configured with us-west-2 access):

```bash
bash lambda/deploy.sh
```

## Structure

```
cli.js              CLI entry point (Bun)
lib/                Shared detection library (browser + Node/Bun)
  index.js          detect() async generator + re-exports
  worker.js         Web Worker wrapper with automatic clustering
  stac.js           STAC search with pagination and deduplication
  cog.js            COG I/O, block enumeration, per-image orchestration
  detect.js         Pure detection algorithm (all thresholds and filters)
  cluster.js        Cross-date spatial clustering
  geo.js            UTM/WGS84 coordinate conversions
  vendor/           Vendored geotiff.js (browser) + environment-aware ESM wrapper
lambda/             AWS Lambda (single-scene detection in us-west-2)
  handler.js        Lambda handler wrapping detectImage
  deploy.sh         One-command deployment
data/               Detection output (CSV with WKT geometry)
```
