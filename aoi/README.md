# aoi — areas of interest

Site catalogues that drive detection runs. The s2-flares CLI is the single
entrypoint: `--aoi <file.geojson>` runs detection over every feature (its geometry
bounds + `--buffer` km is the search box; the feature's `id`/`name` properties tag
the output). Any GeoJSON works — the **standard AOI schema** is just a
FeatureCollection whose features carry `id` and `name` properties.

Each dataset is two files kept side by side: the raw vendor source, and a small
**DuckDB `.sql`** that fits it to the AOI schema. The SQL carries all the dataset
quirks (filtering, dedup, geometry) so the CLI stays generic. A `.sh` builds the
AOIs and kicks off the run; the built `.geojson` is what ships to the box.

For most targeting you no longer need a curated file here at all:
`cloud/emissions.sh aoi` builds a standard AOI geojson straight from the ch4id
features catalogue on the store — by `k=v` filters (`kind=lng_terminal,
status=operating,dataset=gem`) or by any provider's feature ids
(`GEM:…`, `OGIM:…`, `OSM:…`, `MPS:…`).

## What's here

| AOI | scope | used for |
|---|---|---|
| `lng-terminals.geojson` | every global LNG export terminal (81) | the full bulk run |
| `lng-select.geojson` | a curated handful (26) | the routine "run a few terminals" workflow |
| `ras-laffan-das-island.geojson` | 4 hand-picked Gulf flares | the smoke-test / archive fixture |

Each is driven the same way (`--aoi <file>`); they differ only in how many features
they carry. The first is generated from a vendor catalogue (below); the latter two
are small curated subsets.

## LNG export terminals (global)

| file | role |
|---|---|
| `lng-terminals-2025-09.geojson` | raw source — Global Energy Monitor [Global Gas Infrastructure Tracker](https://globalenergymonitor.org/projects/global-gas-infrastructure-tracker/), LNG Terminals, 2025-09 (1198 features, one point per train) |
| `lng-terminals.sql` | transform → standard AOIs (export-only, train dedup, envelopes) |
| `lng-terminals.geojson` | built AOIs: 81 export terminals, one padded-envelope polygon each |
| `lng-terminals.sh` | run it: build the AOIs, then detect (locally, or ship to the box) |

**Dedup.** GEM lists each liquefaction train/unit separately, but all units of a
terminal share a `ProjectID`. The SQL groups by it and emits the bounding envelope
of a terminal's units — so an N-train terminal is **one** AOI, not N overlapping
boxes (463 export features → 253 terminals; 81 under the default built-status
filter). Widen the SQL's status `IN (...)` list for proposed/cancelled too.

```sh
bash aoi/lng-terminals.sh                 # build AOIs (duckdb) + detect locally
# or, for the global run, on the EU-sovereign box:
cloud/box.sh run --aoi aoi/lng-terminals.geojson --start 2025-01-01 --end 2025-12-31
```

The run is recall-first (`LOOSE`); each scene's detections land at
`<out>/<ProjectID>/<mgrs>_<date>.csv` (file presence == done → resumable). On the
box, `box.sh archive` then grows the per-tile parquet archive on object storage.
Quality scoring is downstream — derive the cluster view (`s2-flares cluster
--archive …`, same `core` score) or query the archive in DuckDB. Tunables are env
vars in the `.sh` (`START`/`END`, `OUT`, `SOURCE`, `S2_CONCURRENCY`, …).

## Selection & test AOIs

`lng-select.geojson` is the small AOI behind the routine "kick off a run over a few
interesting terminals" workflow. `lng-select.sh` picks them out of the global
`lng-terminals.geojson` — edit its `NAMES`/`REGIONS` and re-run to repick:

```sh
bash aoi/lng-select.sh                    # repick the subset → lng-select.geojson
cloud/box.sh launch --aoi aoi/lng-select.geojson --start 2025-01-01 --end 2026-06-30
```

`ras-laffan-das-island.geojson` is a hand-written 4-feature fixture (the Qatar +
Das Island Gulf flares) — the smoke test for a fast end-to-end run and the seed for
the archive's reference data. No generator; it's small enough to keep by hand.
