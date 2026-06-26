# aoi — areas of interest

Site catalogues that drive detection runs. The s2-flares CLI is the single
entrypoint: `--aoi <file.geojson>` runs detection over every feature (its geometry
bounds + `--buffer` km is the search box; the feature's `id`/`name` properties tag
the output). Any GeoJSON works — the **standard AOI schema** is just a
FeatureCollection whose features carry `id` and `name` properties.

Each dataset is two files kept side by side: the raw vendor source, and a small
**DuckDB `.sql`** that fits it to the AOI schema. The SQL carries all the dataset
quirks (filtering, dedup, geometry) so the CLI stays generic.

## LNG export terminals (global)

| file | role |
|---|---|
| `lng-terminals-2025-09.geojson` | raw source — Global Energy Monitor [Global Gas Infrastructure Tracker](https://globalenergymonitor.org/projects/global-gas-infrastructure-tracker/), LNG Terminals, 2025-09 (1198 features, one point per train) |
| `lng-terminals.sql` | transform → standard AOIs (export-only, train dedup, envelopes) |
| `lng-terminals.geojson` | built AOIs: 81 export terminals, one padded-envelope polygon each |
| `lng-terminals.sh` | run it: build the AOIs, then fan the CLI out to the Lambda |

**Dedup.** GEM lists each liquefaction train/unit separately, but all units of a
terminal share a `ProjectID`. The SQL groups by it and emits the bounding envelope
of a terminal's units — so an N-train terminal is **one** AOI, not N overlapping
boxes (463 export features → 253 terminals; 81 under the default built-status
filter). Widen the SQL's status `IN (...)` list for proposed/cancelled too.

```sh
bash lambda/deploy.sh          # once, if the detector Lambda isn't deployed
bash aoi/lng-terminals.sh      # build AOIs (duckdb) + fan out to the Lambda (loose)
```

The run is recall-first (`LOOSE`); each scene's detections land at
`s3://$S3_BUCKET/lng/<ProjectID>/<mgrs>_<date>.csv` (atomic writes → resumable).
Quality scoring is downstream (`lib/score.js`). Tunables are env vars in the `.sh`
(`START`/`END`, `S3_BUCKET`, `S2_CONCURRENCY`, …). To preview locally without the
Lambda, point the CLI at a few features: `bun cli.js --aoi <subset>.geojson
--preset loose --out out.csv`.
