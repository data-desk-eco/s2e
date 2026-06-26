#!/usr/bin/env bun

// s2-flares CLI — Sentinel-2 SWIR flare detection over one or more AOIs.
//
// One area, or many: the same detector core, two ways in —
//   --bbox W,S,E,N        a single area
//   --aoi sites.geojson   a FeatureCollection of AOIs; one detection run per
//                         feature, its bbox = the geometry's bounds + --buffer km.
//                         Each feature's `id`/`name` properties tag the output.
// Bring any geojson of sites; per-dataset schema-fitting (dedup, filtering) is the
// job of a small DuckDB .sql kept beside the data — see aoi/.
//
// Detection runs locally (default), or each scene fans out to a deployed Lambda
// (--lambda FN) co-located with the S2 COGs, which writes one CSV per scene to S3
// (atomic → resumable) for large bulk runs. Deploy with lambda/deploy.sh.
//
// Usage: bun cli.js (--bbox W,S,E,N | --aoi file.geojson) [options]

import { searchSTAC } from './lib/stac.js';
import { runAOI } from './lib/run.js';
import { LOOSE, resolveThresholds } from './lib/detect.js';
import { geomBbox, padBbox } from './lib/geo.js';

function parseArgs(argv) {
    const args = { maxCloudCover: 100, concurrency: 4, preset: 'default', buffer: 0,
        region: 'us-west-2', prefix: 'flares' };
    for (let i = 2; i < argv.length; i++) {
        const a = argv[i], next = argv[i + 1];
        switch (a) {
            case '--bbox': args.bbox = next.split(',').map(Number); i++; break;
            case '--aoi': args.aoi = next; i++; break;
            case '--buffer': args.buffer = Number(next); i++; break;
            case '--start': args.start = next; i++; break;
            case '--end': args.end = next; i++; break;
            case '--cloud': args.maxCloudCover = Number(next); i++; break;
            case '--preset': args.preset = next; i++; break;
            case '--out': args.out = next; i++; break;
            case '--concurrency': args.concurrency = Number(next); i++; break;
            case '--min-dates': args.minDates = Number(next); i++; break;
            case '--min-avg-b12': args.minAvgB12 = Number(next); i++; break;
            case '--score-threshold': args.scoreThreshold = Number(next); i++; break;
            case '--lambda': args.lambda = next; i++; break;
            case '--region': args.region = next; i++; break;
            case '--bucket': args.bucket = next; i++; break;
            case '--prefix': args.prefix = next; i++; break;
            case '--help': printUsage(); process.exit(0);
            default: console.error(`Unknown argument: ${a}`); printUsage(); process.exit(1);
        }
    }
    if (!args.bbox && !args.aoi) {
        console.error('Error: provide --bbox W,S,E,N or --aoi file.geojson'); printUsage(); process.exit(1);
    }
    if (args.bbox && args.bbox.length !== 4) {
        console.error('Error: --bbox needs W,S,E,N'); process.exit(1);
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
  bun cli.js (--bbox W,S,E,N | --aoi file.geojson) [options]

AOI:
  --bbox W,S,E,N         A single bounding box (west,south,east,north) in WGS84
  --aoi FILE.geojson     A FeatureCollection; one run per feature. Each feature's
                         geometry bounds (+ --buffer km) is its search box; its
                         id/name properties tag the output rows.
  --buffer KM            Halo added around each AOI's bounds (default: 0)

Window & detector:
  --start / --end YYYY-MM-DD   Date range (default: last 6 months)
  --cloud N              Max scene cloud cover % (default: 100)
  --preset default|loose Thresholds. 'loose' = recall-first (spectral mask only,
                         morphological gates off); filter downstream in SQL.
  --concurrency N        Scenes (or invocations) in flight (default: 4)
  --min-dates / --min-avg-b12 / --score-threshold   Local clustering knobs

Lambda fan-out (bulk; default is local detection):
  --lambda FN            Send each scene to this deployed function instead of
                         detecting locally; it writes one CSV per scene to S3.
  --region R             Lambda region (default: us-west-2)
  --bucket B             S3 bucket holding results (enables resume — done scenes
                         are skipped). Defaults to the function's own bucket.
  --prefix P             S3 key prefix; per-AOI it becomes <prefix>/<id>/ (default: flares)

Output (local mode):
  --out FILE             CSV (default: stdout); .csv also writes .parquet via duckdb

Examples:
  bun cli.js --bbox -104,31.5,-103,32.5 --preset loose --out data/permian.csv
  bun cli.js --aoi aoi/lng-terminals.geojson --preset loose --start 2025-01-01 \\
             --end 2025-12-31 --lambda s2-flares-detect --bucket my-bucket --prefix lng
`);
}

// --- AOIs --------------------------------------------------------------------
// geomBbox / padBbox now live in lib/geo.js (shared with the web API).
async function loadAOIs(args) {
    if (args.bbox) return [{ id: 'aoi', name: '', bbox: args.bbox }];
    const gj = JSON.parse(await Bun.file(args.aoi).text());
    return (gj.features || []).map((f, i) => ({
        id: f.properties?.id ?? f.properties?.ProjectID ?? String(i),
        name: f.properties?.name ?? f.properties?.TerminalName ?? '',
        bbox: padBbox(geomBbox(f.geometry), args.buffer),
    }));
}

async function scenes(aoi, args) {
    const items = [];
    for await (const item of searchSTAC(aoi.bbox, args.start, args.end, { maxCloudCover: args.maxCloudCover })) items.push(item);
    return items;
}

// --- local detection ---------------------------------------------------------
// Search + concurrent detect + cluster for one AOI, via the shared pipeline.
function detectAOI(aoi, args, thresholds) {
    return runAOI(aoi.bbox, args.start, args.end, {
        thresholds, concurrency: args.concurrency, maxCloudCover: args.maxCloudCover,
        cluster: { minDates: args.minDates ?? 1, minAvgB12: args.minAvgB12 ?? 0.5,
            scoreThreshold: args.scoreThreshold ?? 0 },
    });
}

// --- Lambda fan-out ----------------------------------------------------------
// Already-present scene CSVs under the prefix (resume; aws CLI auto-paginates).
async function listDone(args) {
    if (!args.bucket) return new Set();
    const p = Bun.spawn(['aws', 's3api', 'list-objects-v2', '--bucket', args.bucket,
        '--prefix', `${args.prefix}/`, '--region', args.region,
        '--query', 'Contents[].Key', '--output', 'text'], { stdout: 'pipe' });
    const out = await new Response(p.stdout).text();
    return new Set(out.split(/\s+/).filter(k => k && k !== 'None'));
}
async function fanoutAOI(aoi, args, thresholds, send, done) {
    const items = (await scenes(aoi, args)).filter(it =>
        !done.has(`${args.prefix}/${aoi.id}/${it.mgrs}_${it.date}.csv`));
    let sent = 0, dets = 0, idx = 0;
    async function worker() {
        while (idx < items.length) {
            const item = items[idx++];
            try {
                const body = await send({ item, bbox: aoi.bbox, screenOverview: false,
                    thresholds, prefix: `${args.prefix}/${aoi.id}` });
                sent++; dets += body.count || 0;
            } catch (err) {
                process.stderr.write(`    ${aoi.id} ${item.mgrs}_${item.date} FAIL: ${err.message}\n`);
            }
        }
    }
    await Promise.all(Array.from({ length: Math.min(args.concurrency, items.length) }, worker));
    process.stderr.write(`  ${aoi.id} ${aoi.name}: ${sent} scenes sent, ${dets} detections\n`);
    return { sent, dets };
}

async function main() {
    const args = parseArgs(process.argv);
    const t0 = Date.now();
    const aois = await loadAOIs(args);
    process.stderr.write(`s2-flares: ${aois.length} AOI(s) | ${args.start} → ${args.end} | preset=${args.preset}${args.lambda ? ` | lambda=${args.lambda}` : ''}\n`);

    // Bulk: fan each scene out to the Lambda (it writes results to S3, not here).
    if (args.lambda) {
        const { LambdaClient, InvokeCommand } = await import('@aws-sdk/client-lambda');
        const client = new LambdaClient({ region: args.region, maxAttempts: 10, retryMode: 'adaptive' });
        const send = async (payload) => {
            const r = await client.send(new InvokeCommand({
                FunctionName: args.lambda, Payload: Buffer.from(JSON.stringify(payload)) }));
            const body = JSON.parse(Buffer.from(r.Payload).toString());
            if (r.FunctionError) throw new Error(body.errorMessage || r.FunctionError);
            return body;
        };
        const thresholds = args.preset === 'loose' ? LOOSE : undefined; // handler resolves over DEFAULTS
        const done = await listDone(args);
        process.stderr.write(`${done.size} scenes already done under ${args.prefix}/\n`);
        let sent = 0, dets = 0;
        for (const aoi of aois) { const r = await fanoutAOI(aoi, args, thresholds, send, done); sent += r.sent; dets += r.dets; }
        process.stderr.write(`\nfan-out complete: ${sent} scenes invoked, ${dets} detections → s3://${args.bucket || '<lambda bucket>'}/${args.prefix}/ (${((Date.now() - t0) / 1000).toFixed(1)}s)\n`);
        return;
    }

    // Local: detect + cluster per-AOI, emit one CSV carrying detection + cluster fields.
    const thresholds = args.preset === 'loose' ? resolveThresholds(LOOSE) : undefined;
    const lines = ['aoi_id,aoi_name,cluster_id,date,max_b12,avg_b12,pixels,det_lon,det_lat,cluster_lon,cluster_lat,cluster_max_b12,cluster_total_score,cluster_date_count,cluster_persistence,cluster_seasonal'];
    let nClusters = 0;
    for (const aoi of aois) {
        const { clusters, scenes: nScenes } = await detectAOI(aoi, args, thresholds);
        process.stderr.write(`  ${aoi.id} ${aoi.name}: ${nScenes} scenes, ${clusters.length} clusters\n`);
        nClusters += clusters.length;
        for (const c of clusters) for (const d of c.detections) lines.push([
            aoi.id, JSON.stringify(aoi.name), c.id, d.date,
            d.max_b12, c.avg_b12, d.pixels, d.lon, d.lat, c.lon, c.lat, c.max_b12,
            c.total_score?.toFixed(3) ?? '', c.date_count, c.persistence ?? '', c.seasonal,
        ].join(','));
    }
    process.stderr.write(`\n${nClusters} clusters across ${aois.length} AOI(s)\n`);

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
