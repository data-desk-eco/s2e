import { epsgFromMgrs } from './geo.js';

// STAC source profiles. The methodology is identical; only the catalogue's
// endpoint, per-band asset keys, and epsg exposure differ. 'aws' (Element84
// Earth-Search, COG hrefs) is the default and unchanged; 'cdse' is the
// Copernicus Data Space (eopf STAC, raw .jp2 on s3://eodata) for the
// EU-sovereign CloudFerro path — its hrefs are read by lib/cog-gdal.js.
const SOURCES = {
    aws: {
        api: 'https://earth-search.aws.element84.com/v1',
        epsg: it => it.properties['proj:epsg'],
        bands: it => ({ b12: it.assets.swir22?.href, b11: it.assets.swir16?.href,
                        b8a: it.assets.nir08?.href, scl: it.assets.scl?.href }),
    },
    cdse: {
        api: 'https://stac.dataspace.copernicus.eu/v1',
        epsg: it => epsgFromMgrs(it.properties['grid:code']),
        bands: it => ({ b12: it.assets.B12_20m?.href, b11: it.assets.B11_20m?.href,
                        b8a: it.assets.B8A_20m?.href, scl: it.assets.SCL_20m?.href }),
    },
};

async function fetchWithRetry(url, options, maxRetries = 3) {
    for (let attempt = 0; attempt <= maxRetries; attempt++) {
        let resp;
        try {
            resp = await fetch(url, options);
        } catch (err) {
            if (attempt === maxRetries) throw err;
            const delay = (1000 * Math.pow(2, attempt)) * (1 + Math.random() * 0.5);
            console.warn(`fetch retry ${attempt + 1}/${maxRetries} after network error, waiting ${Math.round(delay)}ms`);
            await new Promise(r => setTimeout(r, delay));
            continue;
        }
        if (resp.ok) return resp;
        if (resp.status !== 429 && resp.status < 500) {
            throw new Error(`HTTP ${resp.status}`);
        }
        if (attempt === maxRetries) {
            throw new Error(`HTTP ${resp.status} after ${maxRetries + 1} attempts`);
        }
        let delay;
        if (resp.status === 429) {
            const ra = resp.headers.get('Retry-After');
            const raSec = ra ? parseInt(ra, 10) : NaN;
            delay = (!isNaN(raSec) && raSec > 0) ? Math.min(raSec, 30) * 1000 : (1000 * Math.pow(2, attempt));
        } else {
            delay = 1000 * Math.pow(2, attempt);
        }
        delay *= (1 + Math.random() * 0.5);
        console.warn(`fetch retry ${attempt + 1}/${maxRetries} after HTTP ${resp.status}, waiting ${Math.round(delay)}ms`);
        await new Promise(r => setTimeout(r, delay));
    }
}

export async function* searchSTAC(bbox, start, end, { signal, maxCloudCover = 100, source = 'aws' } = {}) {
    const src = SOURCES[source] ?? SOURCES.aws;
    let startDate = start;
    let endDate = end;
    if (!startDate || !endDate) {
        const now = new Date();
        const sixMonthsAgo = new Date(now);
        sixMonthsAgo.setMonth(sixMonthsAgo.getMonth() - 6);
        startDate = sixMonthsAgo.toISOString().slice(0, 10);
        endDate = now.toISOString().slice(0, 10);
    }
    const payload = {
        collections: ['sentinel-2-l2a'],
        bbox,
        datetime: `${startDate}T00:00:00Z/${endDate}T23:59:59Z`,
        limit: 100
    };
    let items = [];
    let url = `${src.api}/search`;
    let body = payload;
    while (url) {
        const resp = await fetchWithRetry(url, {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify(body),
            signal
        });
        const data = await resp.json();
        items = items.concat(data.features || []);
        const nextLink = (data.links || []).find(l => l.rel === 'next');
        if (nextLink && nextLink.body) {
            url = nextLink.href;
            body = nextLink.body;
        } else {
            url = null;
        }
    }
    // Deduplicate by MGRS tile + date: keep lowest cloud cover per tile per date.
    const byTileDate = {};
    for (const item of items) {
        const dt = item.properties.datetime.slice(0, 10);
        const cloud = item.properties['eo:cloud_cover'] ?? 100;
        const tile = item.properties['grid:code'] || item.properties['s2:mgrs_tile'] || item.id;
        const key = `${tile}_${dt}`;
        if (!byTileDate[key] || cloud < byTileDate[key].cloud) {
            byTileDate[key] = { item, cloud };
        }
    }
    for (const { item } of Object.values(byTileDate)) {
        const cloud = item.properties['eo:cloud_cover'] ?? 100;
        if (cloud > maxCloudCover) continue;
        yield {
            id: item.id,
            date: item.properties.datetime.slice(0, 10),
            cloudCover: item.properties['eo:cloud_cover'],
            mgrs: item.properties['grid:code']?.replace('MGRS-', ''),
            epsg: src.epsg(item),
            bbox: item.bbox,
            // Solar geometry at scene acquisition time. Used downstream to discriminate
            // sun glint (specular geometry, sun-angle-dependent) from real thermal sources
            // (sun-angle-independent). Element84 STAC exposes these on every L2A item.
            sunElevation: item.properties['view:sun_elevation'] ?? null,
            sunAzimuth: item.properties['view:sun_azimuth'] ?? null,
            bands: src.bands(item),
        };
    }
}
