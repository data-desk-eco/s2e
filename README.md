# s2-flares

A **Sentinel-2 SWIR gas-flare detection** methodology core. It detects
flares at 20 m from Sentinel-2 L2A Band 12/11/8A imagery, clusters detections
across dates into persistent sites, and attaches a vision-validated quality score.

It is a small **Rust workspace**: one pure methodology core (`core/`) that compiles
both to a fast native CLI (`cli/`, GDAL-backed) and to WebAssembly (`wasm/`) for the
browser. The same frozen methodology drives every consumer — native and browser
detect and cluster through one shared core, so there is no second implementation
to drift.

```
core/   pure compute — detect, cluster, score, coverage, geo. no I/O. → wasm + cli.
cli/    native gdal cli: STAC search + JP2/COG reads + rayon fan-out + duckdb I/O.
wasm/   wasm-bindgen shim: detectBlock / cluster / scoreCluster for the browser.
```

## What it produces

Two artifacts, and the key design decision is the relationship between them:

- **Detections** are the archive — the source of truth. One row per detection,
  carrying the full discriminating metric set so any gate is reconstructable
  downstream: `max_b12, avg_b12, peak_b11, b12_b11_ratio, peakedness, pixels,
  radiance, saturated, sun_elevation, sun_azimuth, glint_angle, glint_score`.
  `pixels` is the flare HOT-CORE area (combustion-hot pixels grown from the peak,
  not the loose mask's whole-facility flood) and `radiance` is its integrated SWIR
  excess — together a volume signal that keeps ranking flares once `max_b12` saturates.
  Resumability is the per-scene CSV layer (one `<mgrs>_<date>.csv`, presence == done
  → resumable); the published archive is a per-tile rollup of those CSVs,
  `detections/mgrs=…/data.parquet` (date a column, not a path level) — one parquet
  per MGRS tile, ~10² objects of useful size rather than ~10⁴ tiny per-scene files.
  AOI-agnostic: a flare at `(lon,lat)` on a date is a fact independent of the viewport
  that surfaced it (the rollup `DISTINCT`s across overlapping AOIs).

- **Clusters** are a derived **view**, never the source of truth. Clustering is a
  pure function of `(detections, viewport, date-range, thresholds)`, so it is run on
  read — by the web map (in wasm, over raw detections it pulls off `detections/`) for
  any window, and as a stored full-window snapshot (`clusters/data.parquet`)
  co-produced with the rollup for cheap initial pins. One row per cluster + a nested
  `detections` list column, so a reader can column-project the scalar fields for
  cheap map pins and only fetch the array for drill-down.

`cluster_detections` attaches a single **vision-validated quality score**:

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
is also attached.

## CLI

Two subcommands; one frozen methodology. The detector defaults are recall-first —
the full spectral mask runs, the morphological size gates are neutralised (precision
is applied downstream at cluster/score time). Tighten any single variable with a
`--*-min` flag (`--b12-min`, `--b11-min`, `--peak-b12-min`, `--contrast-ratio`,
`--background-floor`, `--peakedness-min`) when you want a leaner archive.

```bash
make                                             # → target/release/s2-flares

# grow the DETECTION archive: one csv per scene, resumable
s2-flares detect --bbox -104,31.5,-103,32.5 --out out/permian
s2-flares detect --bbox -104,31.5,-103,32.5 --contrast-ratio 3.0 --out out/tight
s2-flares detect --aoi aoi/lng-terminals.geojson --source cdse --out out/lng

# derive the cluster VIEW (geojson for a journalist, or nested parquet for the web map)
s2-flares cluster --bbox 51.44,25.84,51.62,25.98 --start 2025-01-01 --end 2025-03-01
s2-flares cluster --archive 's3://bkt/detections/**/*.parquet' --out clusters.parquet
```

`--source aws` reads Element84 COGs (`/vsicurl`); `--source cdse` reads Copernicus
`eodata` JP2 (`/vsis3`, with the N0400 BOA offset harmonised). DuckDB owns the
parquet/S3 I/O for `cluster`; Rust owns the clustering.

## WebAssembly

`wasm/` exposes the core to JS via `wasm-bindgen` (`detectBlock`, `cluster`,
`scoreCluster`). I/O stays JS glue (browser byte-range fetches → typed arrays in);
only the compute crosses. Build with `wasm-pack build wasm/`.

## CloudFerro (EU-sovereign bulk path)

`cloud/box.sh` is the whole pipeline on a CloudFerro WAW3-2 box co-located with
the Copernicus `eodata` archive: `up` (provision) → `run <detect args>` (detached,
resumable native detection) → `archive` (roll the per-scene CSVs up into both
`detections/` per-tile parquet and the `clusters/` view, in one pass on object
storage) → `pull` / `down`; `publish` makes the archive a DuckDB-wasm
web-map backend (public-read + CORS). The box builds and runs the native binary;
it stays disposable, the S3 archive persists.

## Tests

```bash
cargo test -p s2-flares-core    # the methodology unit suite (score, glint, cluster, geo)
```

The pure-compute tests are the parity gate the methodology must not drift from —
each gate of the spectral mask, the score formula, the cluster id hash, and the
glint discriminator is pinned by a unit test.
