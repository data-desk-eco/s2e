# `emissions.sh` — the central bulk emissions detection system

One fleet, three detectors, one store. `emissions.sh` sources `box.sh` (the fleet
primitives below) and dispatches any subset of the Data Desk detectors over one
targeting AOI:

- **`flares`** — s2-flares SWIR flaring (this repo's native CLI, already on every box)
- **`mars`** — MARS-S2L Sentinel-2 methane ML (`~/Research/mars-s2l`, rsynced payload)
- **`hypergas`** — EMIT hyperspectral methane (`~/Tools/hypergas`, rsynced payload;
  needs an Earthdata `~/.netrc`)

plus **ch4id attribution** of the methane detections, run on the head box.

```bash
# targeting: the ch4id features catalogue on the store → aoi geojson.
# filters (kind/status/dataset/…) or ANY provider's feature ids (GEM:/OGIM:/OSM:/MPS:)
./emissions.sh aoi 'kind=lng_terminal,status=operating,dataset=gem' > lng.geojson
./emissions.sh aoi 'GEM:G100002054200,OGIM:123730' > two.geojson

./emissions.sh run -d mars,flares --aoi lng.geojson --start 2026-06-01 --end 2026-07-17
./emissions.sh status                       # per-box, per-detector progress
./emissions.sh archive                      # gather → head → merge into the live store
./emissions.sh attribute 20                 # ch4id over unattributed datadesk plumes
./emissions.sh down

# standing daily incremental run on the head box (detect → merge → attribute):
./emissions.sh cron 'kind=lng_terminal,status=operating,dataset=gem'
```

Everything is detached and resumable (presence == done at the per-scene level), so a
`run` can be left for days and re-issued idempotently; `archive` merges into the live
store objects (`detections/mgrs=…`, `mars-s2l/results.parquet`,
`hypergas/results.parquet`), so results accrue sequentially run after run. Detector
specifics live one-per-file in `detectors/<name>.sh` (`_prep`/`_cmd`/`_merge`/
`_pull`/`_count`); adding a detector is one new file. Unknown subcommands fall
through to `box.sh` (`up`, `down`, `cost`, `ssh`, `publish`, …).

# `box.sh` — CloudFerro fleet orchestration

One script, one auth path, run end-to-end on CloudFerro WAW3-2 boxes co-located with
the Copernicus `eodata` archive (free in-region JP2 reads). Bulk runs fan out across a
FLEET (default 4) of VMs in parallel — the `--aoi` sharded one slice per member; a
bbox/no-aoi run can't be split, so it forces a single box.

## Subcommands

| command | what it does |
|---|---|
| `image` | bake the golden disk image once (full cold build → snapshot); later `up`s boot from it in <1 min |
| `up` | provision the fleet (keypair / secgroup / net / VMs / floating IPs) |
| `run <detect args>` | shard the `--aoi`, detached resumable detect on every member |
| `pull` | rsync every member's per-scene CSVs down to `$LOCAL_DATA` |
| `archive` | gather all members' CSVs+`.cld` to the head, roll up to `s3://$BUCKET/{detections,clouds,clusters}`, refresh `coverage.geojson` |
| `verify` | prove every AOI feature was scanned + 0 errored scenes (per member) |
| `publish` | make the archive a web-map backend: anonymous public-read + CORS so DuckDB-wasm can range-read it |
| `coverage [aoi.geojson]` | (re)build `s3://$BUCKET/coverage.geojson` from the live shards, or from a local AOI file — the scanned-extent overlay |
| `cost` | estimate run cost so far (FLEET × uptime × flavor €/h) |
| `down` | scale to zero (delete every VM + floating IP) |
| `launch <detect args>` | `up` → `run`, detached — kick off the fleet and walk away (boxes stay up; finish later with `archive`/`publish`/`down`) |
| `all <detect args>` | `up` → `run` → `verify` → `archive` → `pull` → `down`, hands-off |
| `ssh [i]` / `ip` / `watch` | interactive login to member `i` / floating IPs / re-attach to the run stream |

`FLEET=N` (default 4) sizes a bulk run; `GPU=1` selects the GPU box line.

## The shared store (`store.sh`)

The archive lands in the central datadesk bucket
(`s3://datadesk-archive` on WAW3-2, defined once in `cloud/store.sh` with a
`store_creds` helper the sibling repos source too). Every repo owns one prefix:
s2-flares `detections/` + `clusters/` + `clouds/` (private) + `coverage.geojson`,
burnoff `vnf/data.parquet`, firedamp `plumes/data.parquet`, ch4id
`features/data.fgb` + `ch4id/` (durable plumes/attributions state), mars-s2l
`mars-s2l/`, hypergas `hypergas/`. Bucket-level config (public-read policy + CORS)
is applied only by `box.sh publish`; the other repos just PUT their objects.

## Auth

`run`/`pull`/`watch` are ssh-only; `up`/`down`/`archive`/`ip`/`coverage` use the
OpenStack API via the vendored 2FA openrc. Non-interactive when a gitignored `.env`
(repo root or `cloud/`) sets `CLOUDFERRO_PASSWORD` + `CLOUDFERRO_TOTP_SECRET` (the
base32 authenticator seed); we feed the password and an `oathtool` code into the
openrc prompts. Quote `.env` values (`CLOUDFERRO_PASSWORD='p@ss$word'`) so the shell
reads them verbatim. The boxes read `eodata` with per-VM creds pulled from the
metadata service at boot (cloud-init), so detection itself needs no secrets.

## Fleet model

Members are `$VM-0 … $VM-(FLEET-1)`, each with its own floating IP cached in
`.box-ip-<i>`. `run` pins the active member count in `.fleet` (a bbox run writes 1);
every op that post-dates it reads `.fleet` so the fan-out matches the boxes that ran.
`run` shards the `--aoi` round-robin (`fs[i::n]`) → balanced slices; a tile two
terminals share landing on two boxes is fine — each writes its own `<id>/` dir and the
rollup dedups. The detect is `nohup`-detached and resumable, so the local `watch`
stream is severable (Ctrl-C / close the session) without stopping the runs.

The golden image (`image`) boots one stock box, lets cloud-init do the full cold
install+build, strips its per-VM creds + cloud-init state, snapshots the disk to
`$BASEIMG`, and tears the box down. Afterwards every `up` boots from `$BASEIMG`
(cloud-init's guards no-op against the on-disk toolchain/tree), so a member is ready in
under a minute — only the per-VM creds + `start_member`'s incremental rebuild run live.

## Archive artifacts

`archive` rolls the per-scene CSVs up into the published parquet, in one head-side pass
(the head gathers every member's files first, so a single rollup sees the whole archive
— a clean per-tile DISTINCT, no cross-box last-write-wins when adjacent terminals share
a tile):

- **`detections/`** — the raw archive: hive parquet `detections/mgrs=…/data.parquet`,
  one deterministic-key file *per tile* (not per scene). Each tile file is
  `SELECT DISTINCT` over every AOI's CSVs for that tile (cross-AOI/cross-shard union +
  dedup), `ORDER BY date` so row-group date stats prune within a file.
- **`clouds/`** — the cloud mask: an immutable per-run parquet collection under
  `clouds/runs/` (plus the legacy `clouds/data.parquet`), with rows
  `glon,glat,date,cloud_frac`, emitted *during* detection over every scene (incl.
  flareless/cloudy — the clear-but-unlit looks that are the honest persistence
  denominator). The run key hashes its scene paths, so retrying the same archive is an
  idempotent PUT. This avoids a global `DISTINCT` rewrite that required tens of GB of
  scratch; the cluster join already set-deduplicates dates per cell across objects.
  AOI-agnostic and internal: **not web-published.**
- **`clusters/`** — the derived view: `s2-flares cluster` over the fresh `detections/`,
  with the persistence denominator joined from `clouds/` → `clusters/mgrs=…/data.parquet`,
  one file per tile (each cluster carries its anchor's tile; one row/cluster + nested
  detections). The web map re-clusters live in wasm. Pure DuckDB on the project bucket
  (`S2_S3_*`) — the `clouds/` join needs no eodata/gdal, so no second credential set.
- **`coverage.geojson`** — the scanned-extent overlay (the web map's coverage outline +
  its archive-vs-detect test). One Polygon per AOI feature that ran, keyed by feature
  `id` and stamped `{name, start, end, scanned}`. `archive` merges the current run's
  features into the existing object **by id** — re-scanning a terminal replaces its
  entry, new AOIs append, nothing duplicates — so coverage grows monotonically across
  runs with no manual bookkeeping. Seed/rebuild standalone with
  `START=… END=… [SCANNED=…] ./box.sh coverage aoi/<file>.geojson` (`SCANNED` overrides
  the stamped scan date, e.g. to backfill a historical run).

`detections` carry the raw points but only positives — a blank region is ambiguous
(scanned-clean vs never-scanned), so coverage can't be derived from them. The AOI
boxes are the authoritative scanned footprint; `coverage.geojson` is that footprint.

## verify

Born of a run that silently hid 25 of 81 LNG terminals from the published archive. Two
assertions over each box's `OUT`, per member, before the gather/archive:

1. **coverage** — every AOI feature in the member's shard was reached. detect writes
   `OUT/<id>/<mgrs>_<date>.csv` (header-only even when flareless), so a feature with no
   subdir was never scanned. id precedence mirrors `load_aois()` in `main.rs`.
2. **no errors** — every attempted scene succeeded. a read/detect FAIL leaves a sibling
   `<mgrs>_<date>.err` (cleared on a later successful retry); any remaining `.err` means
   a scene is unproven. the path to green is just: re-run (resumable) until clean.

`all` gates the teardown on `verify`: `down` fires only once every AOI feature is proven
scanned — a gap keeps the fleet up for a resumable re-run.

## GPU path & parity

`GPU=1` selects the full-tile nvJPEG2000 path: GPU cloud-init + an L40S vGPU flavor +
the NVIDIA driver image, and `run`/`parity` build `--features gpu`. Override
`FLAVOR`/`IMAGE`/`RATE` for another GPU line. `parity` (GPU box only) asserts the
nvJPEG2000 detections equal the GDAL/OpenJPEG detections byte-for-byte over real scenes
(lossless JP2 → identical pixels → identical core output): set `PARITY_BBOX` to a small
test region, optionally narrow with `PARITY_TILE`/`START`/`END`.
