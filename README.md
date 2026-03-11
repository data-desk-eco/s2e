# s2-flares

Sentinel-2 SWIR flare detection library. Detects gas flares at 20m resolution using Sentinel-2 L2A Band 12/11/8A imagery via Cloud Optimized GeoTIFFs from the Element84 STAC catalog.

Pure ES modules, zero npm dependencies. Vendored geotiff.js for COG reads.

## Usage

### Async generator (main thread or worker)

```js
import { detect } from './index.js';

for await (const event of detect(bbox, startDate, endDate, { signal })) {
    // event.type: 'image-start' | 'detections' | 'image-done' | 'progress'
}
```

### Web Worker

```js
const w = new Worker('worker.js', { type: 'module' });
w.postMessage({ type: 'detect', bbox, start, end, clusterOptions: { minDates: 1 } });
w.onmessage = (e) => { /* detections | progress | clusters | error | done */ };
```

### Lower-level API

```js
import { searchSTAC } from './stac.js';
import { detectImage } from './cog.js';
import { clusterDetections } from './cluster.js';

const items = [];
for await (const item of searchSTAC(bbox, start, end)) items.push(item);

const allDetections = [];
for (const item of items) {
    const { detections } = await detectImage(item, bbox);
    allDetections.push(...detections);
}

const clusters = clusterDetections(allDetections, { minDates: 4, minAvgB12: 0.85 });
```

## Files

| File | Purpose |
|------|---------|
| `index.js` | Public API: `detect()` async generator + re-exports |
| `worker.js` | Web Worker wrapper with automatic clustering |
| `stac.js` | STAC search with pagination and deduplication |
| `cog.js` | COG I/O, block enumeration, per-image orchestration |
| `detect.js` | Pure detection algorithm (all thresholds and filters) |
| `cluster.js` | Cross-date spatial clustering |
| `geo.js` | UTM/WGS84 coordinate conversions |
