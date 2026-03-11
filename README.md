# s2-flares

Sentinel-2 SWIR flare detection library. Detects gas flares at 20m resolution using Sentinel-2 L2A Band 12/11/8A imagery via Cloud Optimized GeoTIFFs from the Element84 STAC catalog.

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
const w = new Worker('worker.js');
w.postMessage({ type: 'detect', bbox, start, end });
w.onmessage = (e) => { /* detection | progress | clusters | error | done */ };
```
