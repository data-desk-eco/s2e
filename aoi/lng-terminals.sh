#!/usr/bin/env bash
# Global LNG-export-terminal AOIs: complement the Permian data with every LNG
# export terminal worldwide (Global Energy Monitor GGIT). The DuckDB .sql does all
# schema-fitting (export-only, train dedup, padded envelopes); the s2-flares cli
# does the detection. For a global run, ship the geojson to the EU-sovereign box
# (`cloud/box.sh run --aoi aoi/lng-terminals.geojson`); run locally it detects
# against the public AWS COGs instead.
set -euo pipefail
cd "$(dirname "$0")/.."

rm -f aoi/lng-terminals.geojson
duckdb < aoi/lng-terminals.sql                       # -> aoi/lng-terminals.geojson

exec "${S2:-cargo run --release -q -p s2-flares-cli --}" detect \
  --aoi aoi/lng-terminals.geojson --preset loose --source "${SOURCE:-aws}" \
  --start "${START:-2025-01-01}" --end "${END:-2025-12-31}" \
  --out "${OUT:-out/lng}" --concurrency "${S2_CONCURRENCY:-8}"
