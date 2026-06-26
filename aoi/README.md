# aoi — areas of interest for bulk runs

Region definitions that drive large-area Lambda collection runs. The Permian basin
run uses a single bbox (permian-flaring's `config.sh`); broader runs derive many
AOIs from a point catalogue here.

## LNG export terminals (global)

`lng-terminals-2025-09.geojson` — Global Energy Monitor [Global Gas Infrastructure
Tracker](https://globalenergymonitor.org/projects/global-gas-infrastructure-tracker/),
LNG Terminals, 2025-09 release. 1198 features (export + import). One point per
liquefaction train/unit.

`collect-lng.sh` → `collect.mjs` fan the detection Lambda out over every **export**
terminal:

- **Dedup.** All trains of a terminal share a GEM `ProjectID`, so we group by it
  and scan the padded (~3 km) envelope of a terminal's units **once** — an N-train
  terminal is one AOI, not N overlapping boxes. 463 export features → 253 terminals.
- **Status filter.** Defaults to physically-built statuses
  (`operating,construction,idled,mothballed,retired`); set `STATUS=all` to include
  proposed/cancelled/shelved too.
- **Preset.** Recall-first `LOOSE`; quality scoring is downstream (see `lib/score.js`).
- **Output.** One CSV per scene at
  `s3://$S3_BUCKET/$S3_PREFIX/<ProjectID>/<mgrs>_<date>.csv` (default prefix `lng`,
  separate from the Permian `s2` prefix). Atomic writes → resumable.

```sh
bash lambda/deploy.sh                 # once, if not already deployed
DRY_RUN=1 bash aoi/collect-lng.sh     # report the plan (AOI + scene counts), no invokes
bash aoi/collect-lng.sh               # run it
```

Tunables (env): `START`/`END`, `STATUS`, `PAD`, `S2_CONCURRENCY`, `LIMIT` (cap AOIs
for testing), `FUNCTION_NAME`, `REGION`, `S3_BUCKET`, `S3_PREFIX`.
