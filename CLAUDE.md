# s2-flares

Shared Sentinel-2 SWIR flare detection library. Extracted from burnoff, consumed by both burnoff (P2P detection) and gaslight (single-flare "Enhance" feature).

## Architecture

Pure ES modules throughout. No build step, no npm dependencies. Workers use `{ type: 'module' }`.

```
index.js          Public API: detect() async generator + re-exports
worker.js         Web Worker wrapper (postMessage interface)
stac.js           STAC search (Element84, async generator with pagination)
cog.js            COG I/O (openCOG, readWindow, enumerateBlocks, detectImage)
detect.js         Pure detection algorithm (typed arrays in, detections out)
cluster.js        Cross-date spatial clustering (pure function)
geo.js            UTM/WGS84 conversions + degree helpers
vendor/
  geotiff.js      Vendored geotiff.js 2.1 (UMD)
  geotiff-esm.js  Thin ESM wrapper for geotiff.js
```

## Key Design Decisions

- **detect.js takes typed arrays, not GeoTIFF images.** I/O is in cog.js; detection is pure computation. This lets burnoff do its own I/O with caching and P2P partitioning.
- **clusterDetections is a pure function** with no global state. Consumers pass `observations` map for persistence calculation, or null to skip.
- **searchSTAC is an async generator** that yields normalized items after deduplication.
- **The worker runs clustering automatically** but consumers can also use the library functions directly (burnoff does this).

## Detection Algorithm

See burnoff CLAUDE.md for the full algorithm spec. The thresholds and logic are identical — s2-flares is the canonical implementation.

## Consumers

- **burnoff**: Uses lower-level functions (searchSTAC, openCOG, readWindow, enumerateBlocks, detectBlock) for P2P block partitioning and CRDT caching. Uses clusterDetections with terminal naming post-processing.
- **gaslight**: Uses the worker.js message interface for single-flare enhancement. Relaxed thresholds (minDates=1, minAvgB12=0.5).
