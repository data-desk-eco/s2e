#!/bin/bash
# standing incremental emissions run — installed on the head box by
# `emissions.sh cron`. detects new scenes over the standing aoi across the
# enabled detectors, merges results into the live archive on the shared store,
# then attributes new methane plumes with ch4id. every stage is resumable and
# idempotent, so the overlapping lookback window re-scans nothing.
set -uo pipefail
cd "$HOME"
set -a; . ./.emissions.env; set +a
export AOI=$HOME/_cron_aoi.geojson SITES=$HOME/_cron_sites.csv
export START=$(date -u -d "-${LOOKBACK:-14} days" +%F) END=$(date -u +%F)
export SHARD=0 NSHARDS=1
echo "=== daily $START..$END dets=$DETS $(date -u +%FT%TZ)"
for d in ${DETS//,/ }; do
  bash "run-$d.sh" || { echo "daily: $d run failed" >&2; continue; }
  RUN=cron bash "merge-$d.sh" || echo "daily: $d merge failed" >&2
done
[ -d ch4id ] && { cd ch4id && bin/box run -n "${ATTR_N:-10}"; } || true
