#!/usr/bin/env bash
# pick a handful of interesting LNG export terminals out of the global catalogue
# (lng-terminals.geojson) into lng-select.geojson — the small AOI that drives our
# routine "kick off a run over a few terminals" workflow. edit the picks below
# (NAMES + region bboxes) and re-run; then ship it:
#   cloud/box.sh launch --aoi aoi/lng-select.geojson --start 2025-01-01 --end 2026-06-30
set -euo pipefail
cd "$(dirname "$0")"
python3 - <<'PY'
import json
src=json.load(open('lng-terminals.geojson'))['features']
NAMES={'Yamal LNG Terminal','Das Island LNG Terminal'}      # by name
REGIONS=[(51,24,52,27),               # ras laffan (qatar complex)
         (-98,25,-89,31),             # us gulf coast (tx + la)
         (113,-45,154,-10)]           # australia (excl. png at -9.3)
def cen(g):
    r=g['coordinates'][0]; xs=[p[0] for p in r]; ys=[p[1] for p in r]
    return (min(xs)+max(xs))/2,(min(ys)+max(ys))/2
def keep(f):
    lon,lat=cen(f['geometry'])
    return f['properties']['name'] in NAMES \
        or any(w<=lon<=e and s<=lat<=n for w,s,e,n in REGIONS)
sel=[f for f in src if keep(f)]
json.dump({'type':'FeatureCollection','features':sel},open('lng-select.geojson','w'))
print(f'{len(sel)} terminals -> aoi/lng-select.geojson')
for f in sorted(sel,key=lambda f:f['properties']['name']): print(' ',f['properties']['name'])
PY
