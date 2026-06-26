#!/usr/bin/env bash
# Run the s2-flares detection Lambda over every global LNG EXPORT terminal
# (Global Energy Monitor GGIT, 2025-09) — the second bulk run, complementing the
# Permian basin collection. Trains/units of one terminal share a GEM ProjectID and
# are collapsed into a single padded envelope, so each terminal is scanned once.
#
# Prereq: the detection Lambda must be deployed (bash lambda/deploy.sh). Output
# lands in s3://$S3_BUCKET/$S3_PREFIX/<ProjectID>/<mgrs>_<date>.csv, resumable.
# Dry-run the plan first:  DRY_RUN=1 bash aoi/collect-lng.sh
set -euo pipefail
cd "$(dirname "$0")/.."

export AOI_GEOJSON="${AOI_GEOJSON:-aoi/lng-terminals-2025-09.geojson}"
export FUNCTION_NAME="${FUNCTION_NAME:-s2-flares-detect}"
export REGION="${REGION:-us-west-2}"
export S3_BUCKET="${S3_BUCKET:-s2-flares-$(aws sts get-caller-identity --query Account --output text)}"
export S3_PREFIX="${S3_PREFIX:-lng}"
export START="${START:-2025-01-01}"
export END="${END:-2025-12-31}"
export STATUS="${STATUS:-operating,construction,idled,mothballed,retired}"  # 'all' for every status
export S2_CONCURRENCY="${S2_CONCURRENCY:-16}"

exec node aoi/collect.mjs
