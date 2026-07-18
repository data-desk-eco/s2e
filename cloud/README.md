# CloudFerro control plane

One fleet runs one Rust binary. `s2-flares detect --mode both` searches each L1C
acquisition once, runs CloudSEN and MARS-S2L natively, and reuses the loaded chip
for flare detection. There are no detector plugins, Python payloads or Hypergas
hooks.

```bash
./box.sh launch --mode both --aoi ../aoi/uk-gas-import-terminals.geojson \
  --start 2026-01-01 --end 2026-07-17
./box.sh status
./box.sh watch
./box.sh verify
./box.sh archive
./box.sh pull
./box.sh down
```

`FLEET=N` controls AOI sharding (default four); bbox runs use one member. The
working tree is rsynced, built incrementally and launched as a detached resumable
job. `emissions.sh` remains only as a catalogue-to-AOI helper and compatibility
spelling for the same `box.sh` workflow.

## Persistence and archive

Workers write independent GeoJSON analysis records under
`out/observations/<area>/<scene>/`. Each flare, plume and cloud result is atomic
and keyed by methodology fingerprint. Positive plume probability rasters live in
`out/assets/`. `verify` requires the detector success sentinel, at least one record
for every sharded AOI and no `.err` files.

`box.sh archive` gathers worker outputs onto the head and calls:

```bash
s2-flares archive --input out --destination s3://$BUCKET --views
```

The command publishes `observations/` and `assets/` unchanged, then uses DuckDB to
rebuild disposable views:

- `detections/mgrs=<tile>/data.parquet`: flat flare-query view.
- `clouds/data.parquet`: clear-sky persistence input.
- `plumes/results.parquet`: scene analysis plus one row per plume component.
- `clusters/mgrs=<tile>/data.parquet`: derived by the subsequent native `cluster`
  command for the requested full-window snapshot.
- `coverage.geojson`: accumulated AOI coverage.

Only GeoJSON analysis records and their referenced assets are authoritative.
Deleting and rebuilding the Parquet products loses no detector output.

## Infrastructure

Cloud-init installs Rust, GDAL, DuckDB and awscli. Per-VM `/vsis3/eodata`
credentials are written from metadata at boot. Model files are cached and checked
against hashes pinned in `cli/src/models.rs`. OpenStack operations use the vendored
2FA openrc; `.env` can provide `CLOUDFERRO_PASSWORD` and
`CLOUDFERRO_TOTP_SECRET` for non-interactive operation.
