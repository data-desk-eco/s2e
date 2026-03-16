# s2-flares

Shared Sentinel-2 SWIR flare detection library. Extracted from burnoff, consumed by both burnoff (P2P detection) and gaslight (single-flare "Enhance" feature).

## Architecture

Pure ES modules throughout. No build step. Workers use `{ type: 'module' }`. Browser consumers have zero npm dependencies (vendored geotiff.js). CLI and Lambda use the npm `geotiff` package for Node.js/Bun compatibility.

```
cli.js              CLI entry point (Bun)
lib/
  index.js          Public API: detect() async generator + re-exports
  worker.js         Web Worker wrapper (postMessage interface)
  stac.js           STAC search (Element84, async generator with pagination)
  cog.js            COG I/O (openCOG, readWindow, enumerateBlocks, detectImage)
  detect.js         Pure detection algorithm (typed arrays in, detections out)
  cluster.js        Cross-date spatial clustering (pure function)
  geo.js            UTM/WGS84 conversions + degree helpers
  vendor/
    geotiff.js      Vendored geotiff.js 2.1 (UMD, browser only)
    geotiff-esm.js  Environment-aware wrapper (browser: vendored UMD, Node/Bun: npm)
lambda/
  handler.js        AWS Lambda handler wrapping detectImage (single scene)
  deploy.sh         One-command Lambda deployment to us-west-2
data/               Detection output (CSV with WKT, gitignored)
```

## Key Design Decisions

- **detect.js takes typed arrays, not GeoTIFF images.** I/O is in cog.js; detection is pure computation. This lets burnoff do its own I/O with caching and P2P partitioning.
- **clusterDetections is a pure function** with no global state. Consumers pass `observations` map for persistence calculation, or null to skip.
- **searchSTAC is an async generator** that yields normalized items after deduplication.
- **The worker runs clustering incrementally** after each detection batch (not just at the end), so consumers get live cluster updates. Consumers can also use the library functions directly (burnoff does this).
- **Each cluster has a deterministic `id`** (hash of anchor lat/lon at 4 decimal places) for stable deep linking and caching.

## Detection Algorithm

See burnoff CLAUDE.md for the full algorithm spec. The thresholds and logic are identical — s2-flares is the canonical implementation.

## CLI

Bulk detection over arbitrary bounding boxes. Runs under Bun. Two execution modes:

- **local**: Processes scenes in-process with configurable concurrency. Good for small areas or testing.
- **lambda**: STAC search runs locally, then fans out one Lambda invocation per scene (co-located with S2 COGs in us-west-2 for zero-egress, low-latency access). Good for large areas.

The CLI shares all detection code with the browser library — `detectImage`, `searchSTAC`, `clusterDetections` are the exact same modules. The Lambda handler is 15 lines wrapping `detectImage`.

Output is CSV with WKT geometry (one row per detection, both detection and cluster locations). Compatible with ogr2ogr. Saved to `data/`.

## Lambda

- Function: `s2-flares-detect` in us-west-2 (ARM64/Graviton)
- Runtime: Node.js 22 (AWS doesn't offer Bun as a Lambda runtime)
- Deploy: `bash lambda/deploy.sh` (creates IAM role + function, builds zip)
- Package: ~1.3MB (source + geotiff npm package, no vendored UMD)

## Consumers

- **burnoff**: Uses lower-level functions (searchSTAC, openCOG, readWindow, enumerateBlocks, detectBlock) for P2P block partitioning and CRDT caching. Uses clusterDetections with terminal naming post-processing.
- **gaslight**: Uses the worker.js message interface for single-flare enhancement. Relaxed thresholds (minDates=1, minAvgB12=0.5). Each cluster rendered as a first-class map feature with its own detail card and deep link.
- **cli.js**: Bulk detection for large bounding boxes (Permian Basin, LNG terminals, etc). Uses Lambda for scale.
