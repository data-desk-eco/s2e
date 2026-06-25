#!/usr/bin/env bun

// s2-flares CLI — Sentinel-2 SWIR flare detection over a bounding box.
//
// In-process local detection (good for small/medium areas and testing). For
// large-area bulk collection, deploy the detector as a Lambda co-located with the
// S2 COGs (lambda/deploy.sh) and fan scenes out with an S3-writing collector —
// see the permian-flaring repo's scripts/collect_s2.mjs.
//
// Usage: bun cli.js --bbox W,S,E,N [--start YYYY-MM-DD] [--end YYYY-MM-DD]
//                   [--cloud 50] [--preset default|loose] [--out file.csv]

import { searchSTAC } from './lib/stac.js';
import { detectImage } from './lib/cog.js';
import { clusterDetections } from './lib/cluster.js';
import { DEFAULTS, LOOSE, resolveThresholds } from './lib/detect.js';

function parseArgs(argv) {
    const args = { maxCloudCover: 100, concurrency: 4, preset: 'default' };
    for (let i = 2; i < argv.length; i++) {
        const a = argv[i], next = argv[i + 1];
        switch (a) {
            case '--bbox': args.bbox = next.split(',').map(Number); i++; break;
            case '--start': args.start = next; i++; break;
            case '--end': args.end = next; i++; break;
            case '--cloud': args.maxCloudCover = Number(next); i++; break;
            case '--preset': args.preset = next; i++; break;
            case '--out': args.out = next; i++; break;
            case '--concurrency': args.concurrency = Number(next); i++; break;
            case '--min-dates': args.minDates = Number(next); i++; break;
            case '--min-avg-b12': args.minAvgB12 = Number(next); i++; break;
            case '--score-threshold': args.scoreThreshold = Number(next); i++; break;
            case '--help': printUsage(); process.exit(0);
            default: console.error(`Unknown argument: ${a}`); printUsage(); process.exit(1);
        }
    }
    if (!args.bbox || args.bbox.length !== 4) {
        console.error('Error: --bbox W,S,E,N is required'); printUsage(); process.exit(1);
    }
    if (!args.start || !args.end) {
        const now = new Date();
        const ago = new Date(now); ago.setMonth(ago.getMonth() - 6);
        args.start = args.start || ago.toISOString().slice(0, 10);
        args.end = args.end || now.toISOString().slice(0, 10);
    }
    if (args.preset !== 'default' && args.preset !== 'loose') {
        console.error(`Error: --preset must be 'default' or 'loose'`); process.exit(1);
    }
    return args;
}

function printUsage() {
    console.log(`
s2-flares CLI — Sentinel-2 SWIR flare detection

Usage:
  bun cli.js --bbox W,S,E,N [options]

Options:
  --bbox W,S,E,N        Bounding box (west,south,east,north) in WGS84 degrees
  --start YYYY-MM-DD     Start date (default: 6 months ago)
  --end YYYY-MM-DD       End date (default: today)
  --cloud N              Max scene cloud cover % (default: 100)
  --preset default|loose Detector thresholds. 'loose' = recall-first (spectral
                         mask only, morphological gates neutralised); filter later
                         in SQL. Default reproduces the proven conservative gates.
  --concurrency N        Parallel scene processing (default: 4)
  --min-dates N          Min detection dates per cluster (default: 1)
  --min-avg-b12 N        Min avg B12 per cluster (default: 0.5)
  --score-threshold N    Drop clusters below this quality score (default: off)
  --out FILE             Output CSV (default: stdout). .csv also writes .parquet
                         if duckdb is on PATH.
  --help                 Show this help

Examples:
  bun cli.js --bbox 51.44,25.84,51.62,25.98 --start 2025-01-01 --end 2025-03-01 --out data/ras-laffan.csv
  bun cli.js --bbox -104.0,31.5,-103.0,32.5 --preset loose --cloud 50 --out data/permian.csv
`);
}

async function runLocal(args) {
    const { bbox, start, end, maxCloudCover, concurrency } = args;
    const thresholds = args.preset === 'loose' ? resolveThresholds(LOOSE) : resolveThresholds(DEFAULTS);

    process.stderr.write('Searching STAC catalog...\n');
    const items = [];
    for await (const item of searchSTAC(bbox, start, end, { maxCloudCover })) items.push(item);
    process.stderr.write(`Found ${items.length} scenes (preset=${args.preset})\n`);
    if (items.length === 0) return { detections: [], observations: new Map() };

    const allDetections = [];
    const observations = new Map();
    let processed = 0, idx = 0;

    async function processItem(item) {
        const t0 = Date.now();
        try {
            const result = await detectImage(item, bbox, { thresholds });
            observations.set(item.date, { cloudFree: result.cloudFree });
            if (result.detections.length > 0) allDetections.push(...result.detections);
            processed++;
            const status = result.skippedOverview ? 'skipped' :
                `${result.detections.length} det, ${result.blocksProcessed} blocks`;
            process.stderr.write(`[${processed}/${items.length}] ${item.date} ${item.mgrs} — ${status} (${Date.now() - t0}ms)\n`);
        } catch (err) {
            processed++;
            process.stderr.write(`[${processed}/${items.length}] ${item.date} ${item.mgrs} — ERROR: ${err.message}\n`);
        }
    }
    async function worker() { while (idx < items.length) await processItem(items[idx++]); }
    await Promise.all(Array.from({ length: Math.min(concurrency, items.length) }, worker));
    return { detections: allDetections, observations };
}

async function main() {
    const args = parseArgs(process.argv);
    const t0 = Date.now();
    process.stderr.write(`s2-flares: ${args.bbox.join(',')} | ${args.start} → ${args.end} | preset=${args.preset}\n`);

    const { detections, observations } = await runLocal(args);
    process.stderr.write(`\nDetection complete: ${detections.length} raw detections\n`);

    const clusters = clusterDetections(detections, {
        observations,
        minDates: args.minDates ?? 1,
        minAvgB12: args.minAvgB12 ?? 0.5,
        scoreThreshold: args.scoreThreshold ?? 0,
    });
    process.stderr.write(`Clustered into ${clusters.length} sites\n`);

    // CSV: one row per detection, carrying both detection and cluster fields.
    const lines = ['cluster_id,date,max_b12,avg_b12,pixels,det_lon,det_lat,cluster_lon,cluster_lat,cluster_max_b12,cluster_total_score,cluster_date_count,cluster_persistence,cluster_seasonal'];
    for (const c of clusters) {
        for (const d of c.detections) {
            lines.push([
                c.id, d.date, d.max_b12, c.avg_b12, d.pixels, d.lon, d.lat,
                c.lon, c.lat, c.max_b12, c.total_score?.toFixed(3) ?? '',
                c.date_count, c.persistence ?? '', c.seasonal,
            ].join(','));
        }
    }
    const output = lines.join('\n') + '\n';
    if (args.out) {
        await Bun.write(args.out, output);
        process.stderr.write(`Written to ${args.out}\n`);
        if (args.out.endsWith('.csv')) {
            const parquetPath = args.out.replace(/\.csv$/, '.parquet');
            try {
                const { execSync } = await import('child_process');
                execSync(`duckdb -c "COPY (SELECT * FROM '${args.out}') TO '${parquetPath}' (FORMAT PARQUET, COMPRESSION ZSTD)"`);
                process.stderr.write(`Parquet: ${parquetPath}\n`);
            } catch { /* duckdb not available — CSV is still written */ }
        }
    } else {
        process.stdout.write(output);
    }
    process.stderr.write(`Total time: ${((Date.now() - t0) / 1000).toFixed(1)}s\n`);
}

main().catch(err => { console.error('Fatal:', err.message); process.exit(1); });
