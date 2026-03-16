#!/usr/bin/env bun

// S2 Flare Detection CLI
// Usage: bun cli.js --bbox W,S,E,N [--start YYYY-MM-DD] [--end YYYY-MM-DD] [--cloud 50] [--mode local|lambda] [--out file.geojson]

import { searchSTAC } from './lib/stac.js';
import { detectImage } from './lib/cog.js';
import { clusterDetections } from './lib/cluster.js';

// --- Argument parsing ---

function parseArgs(argv) {
    const args = { mode: 'local', maxCloudCover: 100, concurrency: 4 };
    for (let i = 2; i < argv.length; i++) {
        const arg = argv[i];
        const next = argv[i + 1];
        switch (arg) {
            case '--bbox':
                args.bbox = next.split(',').map(Number);
                i++; break;
            case '--start':
                args.start = next; i++; break;
            case '--end':
                args.end = next; i++; break;
            case '--cloud':
                args.maxCloudCover = Number(next); i++; break;
            case '--mode':
                args.mode = next; i++; break;
            case '--out':
                args.out = next; i++; break;
            case '--concurrency':
                args.concurrency = Number(next); i++; break;
            case '--min-dates':
                args.minDates = Number(next); i++; break;
            case '--min-avg-b12':
                args.minAvgB12 = Number(next); i++; break;
            case '--help':
                printUsage(); process.exit(0);
            default:
                console.error(`Unknown argument: ${arg}`);
                printUsage(); process.exit(1);
        }
    }
    if (!args.bbox || args.bbox.length !== 4) {
        console.error('Error: --bbox W,S,E,N is required');
        printUsage(); process.exit(1);
    }
    // Default date range: last 6 months
    if (!args.start || !args.end) {
        const now = new Date();
        const sixMonthsAgo = new Date(now);
        sixMonthsAgo.setMonth(sixMonthsAgo.getMonth() - 6);
        args.start = args.start || sixMonthsAgo.toISOString().slice(0, 10);
        args.end = args.end || now.toISOString().slice(0, 10);
    }
    return args;
}

function printUsage() {
    console.log(`
s2-flares CLI — Sentinel-2 SWIR flare detection

Usage:
  bun cli.js --bbox W,S,E,N [options]

Options:
  --bbox W,S,E,N       Bounding box (west,south,east,north) in WGS84 degrees
  --start YYYY-MM-DD   Start date (default: 6 months ago)
  --end YYYY-MM-DD     End date (default: today)
  --cloud N            Max cloud cover percentage (default: 100)
  --concurrency N      Parallel scene processing (default: 4)
  --min-dates N        Min detection dates for clustering (default: 4)
  --min-avg-b12 N      Min avg B12 for clustering (default: 0.85)
  --mode local|lambda  Execution mode (default: local)
  --out FILE           Output file (default: stdout)
  --help               Show this help

Examples:
  # Small area — single LNG terminal (Ras Laffan)
  bun cli.js --bbox 51.44,25.84,51.62,25.98 --start 2025-01-01 --end 2025-03-01 --out data/ras-laffan.csv

  # Larger area — Permian Basin slice
  bun cli.js --bbox -104.0,31.5,-103.0,32.5 --start 2025-01-01 --end 2025-02-01 --cloud 50 --out data/permian.csv

  # Full Permian via Lambda (co-located with S3 in us-west-2)
  bun cli.js --bbox -104.5,30.5,-101.0,33.5 --start 2024-07-01 --end 2025-01-01 --mode lambda --out data/permian-full.csv
`);
}

// --- Local execution mode ---

async function runLocal(args) {
    const { bbox, start, end, maxCloudCover, concurrency } = args;

    // Phase 1: STAC search
    process.stderr.write('Searching STAC catalog...\n');
    const items = [];
    for await (const item of searchSTAC(bbox, start, end, { maxCloudCover })) {
        items.push(item);
    }
    process.stderr.write(`Found ${items.length} scenes\n`);

    if (items.length === 0) return { detections: [], observations: new Map() };

    // Phase 2: Process scenes with controlled concurrency
    const allDetections = [];
    const observations = new Map();
    let processed = 0;

    async function processItem(item) {
        const t0 = Date.now();
        try {
            const result = await detectImage(item, bbox);
            observations.set(item.date, { cloudFree: result.cloudFree });
            if (result.detections.length > 0) {
                allDetections.push(...result.detections);
            }
            processed++;
            const elapsed = Date.now() - t0;
            const status = result.skippedOverview ? 'skipped' :
                `${result.detections.length} det, ${result.blocksProcessed} blocks`;
            process.stderr.write(
                `[${processed}/${items.length}] ${item.date} ${item.mgrs} — ${status} (${elapsed}ms)\n`
            );
        } catch (err) {
            processed++;
            process.stderr.write(
                `[${processed}/${items.length}] ${item.date} ${item.mgrs} — ERROR: ${err.message}\n`
            );
        }
    }

    // Simple concurrency pool
    let idx = 0;
    async function worker() {
        while (idx < items.length) {
            const item = items[idx++];
            await processItem(item);
        }
    }
    const workers = [];
    for (let i = 0; i < Math.min(concurrency, items.length); i++) {
        workers.push(worker());
    }
    await Promise.all(workers);

    return { detections: allDetections, observations };
}

// --- Lambda execution mode ---

async function runLambda(args) {
    const { bbox, start, end, maxCloudCover, concurrency } = args;
    const { LambdaClient, InvokeCommand } = await import('@aws-sdk/client-lambda');
    const lambda = new LambdaClient({ region: 'us-west-2' });

    // Phase 1: STAC search (runs locally — lightweight)
    process.stderr.write('Searching STAC catalog...\n');
    const items = [];
    for await (const item of searchSTAC(bbox, start, end, { maxCloudCover })) {
        items.push(item);
    }
    process.stderr.write(`Found ${items.length} scenes — invoking Lambda for each\n`);

    if (items.length === 0) return { detections: [], observations: new Map() };

    // Phase 2: Fan out to Lambda
    const allDetections = [];
    const observations = new Map();
    let processed = 0;

    async function invokeForItem(item, retries = 3) {
        const t0 = Date.now();
        for (let attempt = 0; attempt <= retries; attempt++) {
            try {
                const cmd = new InvokeCommand({
                    FunctionName: 's2-flares-detect',
                    Payload: JSON.stringify({ item, bbox }),
                });
                const resp = await lambda.send(cmd);
                const payload = JSON.parse(Buffer.from(resp.Payload).toString());

                if (resp.FunctionError) {
                    throw new Error(payload.errorMessage || 'Lambda error');
                }

                observations.set(item.date, { cloudFree: payload.cloudFree });
                if (payload.detections?.length > 0) {
                    allDetections.push(...payload.detections);
                }
                processed++;
                const elapsed = Date.now() - t0;
                const status = payload.skippedOverview ? 'skipped' :
                    `${payload.detections?.length || 0} det, ${payload.blocksProcessed} blocks`;
                process.stderr.write(
                    `[${processed}/${items.length}] ${item.date} ${item.mgrs} — ${status} (${elapsed}ms)\n`
                );
                return;
            } catch (err) {
                const isRetryable = err.message?.includes('Rate Exceeded') ||
                    err.message?.includes('TooManyRequestsException') ||
                    err.message?.includes('Task timed out');
                if (isRetryable && attempt < retries) {
                    const delay = 1000 * Math.pow(2, attempt) * (1 + Math.random() * 0.5);
                    process.stderr.write(
                        `  retry ${attempt + 1}/${retries} for ${item.date} ${item.mgrs} in ${Math.round(delay)}ms\n`
                    );
                    await new Promise(r => setTimeout(r, delay));
                    continue;
                }
                processed++;
                process.stderr.write(
                    `[${processed}/${items.length}] ${item.date} ${item.mgrs} — ERROR: ${err.message}\n`
                );
                return;
            }
        }
    }

    // Concurrency pool for Lambda invocations
    let idx = 0;
    async function worker() {
        while (idx < items.length) {
            const item = items[idx++];
            await invokeForItem(item);
        }
    }
    const workers = [];
    for (let i = 0; i < Math.min(concurrency, items.length); i++) {
        workers.push(worker());
    }
    await Promise.all(workers);

    return { detections: allDetections, observations };
}

// --- Main ---

async function main() {
    const args = parseArgs(process.argv);
    const t0 = Date.now();

    process.stderr.write(`s2-flares: ${args.bbox.join(',')} | ${args.start} → ${args.end} | mode=${args.mode}\n`);

    let result;
    if (args.mode === 'lambda') {
        result = await runLambda(args);
    } else {
        result = await runLocal(args);
    }

    const { detections, observations } = result;
    process.stderr.write(`\nDetection complete: ${detections.length} raw detections\n`);

    // Phase 3: Cluster
    const clusterOptions = {
        observations,
        minDates: args.minDates ?? 1,
        minAvgB12: args.minAvgB12 ?? 0.5,
    };
    const clusters = clusterDetections(detections, clusterOptions);
    process.stderr.write(`Clustered into ${clusters.length} sites\n`);

    // Phase 4: Output as CSV (one row per detection, both detection and cluster coords)
    // For ogr2ogr: ogr2ogr out.gpkg in.csv -oo X_POSSIBLE_NAMES=det_lon -oo Y_POSSIBLE_NAMES=det_lat -a_srs EPSG:4326
    const lines = ['cluster_id,date,max_b12,avg_b12,pixels,det_lon,det_lat,cluster_lon,cluster_lat,cluster_max_b12,cluster_avg_b12,cluster_date_count,cluster_persistence,cluster_seasonal'];
    for (const c of clusters) {
        for (const d of c.detections) {
            lines.push([
                c.id,
                d.date,
                d.max_b12,
                c.avg_b12,
                d.pixels,
                d.lon,
                d.lat,
                c.lon,
                c.lat,
                c.max_b12,
                c.avg_b12,
                c.date_count,
                c.persistence ?? '',
                c.seasonal,
            ].join(','));
        }
    }
    const output = lines.join('\n') + '\n';
    if (args.out) {
        await Bun.write(args.out, output);
        process.stderr.write(`Written to ${args.out}\n`);
        // Convert to parquet if duckdb is available
        if (args.out.endsWith('.csv')) {
            const parquetPath = args.out.replace(/\.csv$/, '.parquet');
            try {
                const { execSync } = await import('child_process');
                execSync(`duckdb -c "COPY (SELECT * FROM '${args.out}') TO '${parquetPath}' (FORMAT PARQUET, COMPRESSION ZSTD)"`);
                process.stderr.write(`Parquet: ${parquetPath}\n`);
            } catch {
                // duckdb not available — CSV is still written
            }
        }
    } else {
        process.stdout.write(output);
    }

    const elapsed = Date.now() - t0;
    process.stderr.write(`Total time: ${(elapsed / 1000).toFixed(1)}s\n`);
}

main().catch(err => {
    console.error('Fatal:', err.message);
    process.exit(1);
});
