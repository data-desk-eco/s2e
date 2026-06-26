#!/usr/bin/env bash
# Global LNG-export-terminal run: complement the Permian data with every LNG
# export terminal worldwide (Global Energy Monitor GGIT). The DuckDB .sql does all
# schema-fitting (export-only, train dedup, padded envelopes); the s2-flares CLI
# does the detection, fanning each scene out to the deployed Lambda.
#
# Prereq: the detector Lambda is deployed (bash lambda/deploy.sh).
set -euo pipefail
cd "$(dirname "$0")/.."

rm -f aoi/lng-terminals.geojson
duckdb < aoi/lng-terminals.sql                       # -> aoi/lng-terminals.geojson

exec bun cli.js --aoi aoi/lng-terminals.geojson --preset loose \
  --start "${START:-2025-01-01}" --end "${END:-2025-12-31}" \
  --lambda "${FUNCTION_NAME:-s2-flares-detect}" --region "${REGION:-us-west-2}" \
  --bucket "${S3_BUCKET:-s2-flares-$(aws sts get-caller-identity --query Account --output text)}" \
  --prefix "${S3_PREFIX:-lng}" --concurrency "${S2_CONCURRENCY:-16}"
