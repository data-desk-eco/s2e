#!/usr/bin/env bash
# Compatibility entry point for Sentinel-2 emissions work. The control plane is
# deliberately one native job now: `s2-flares detect --mode both`. Fleet lifecycle,
# sharding, resume, archive and teardown all live in box.sh.
set -euo pipefail
CALLER="$PWD"
cd "$(dirname "$0")"
. "${DATA_DESK:-$HOME/Tools/data-desk}/store.sh"

# Shared infrastructure catalogue → detector AOI. QUERY is comma-separated k=v
# filters or a comma-separated set of feature ids.
aoi(){
  local q=${1:?aoi QUERY: 'kind=lng_terminal[,status=operating]' or 'GEM:id[,id…]'} where
  if [[ $q == *=* ]]; then where="$(sed "s/=/ = '/g; s/,/' AND /g" <<<"$q")'"
  else where="id IN ('$(sed "s/,/','/g" <<<"$q")')"; fi
  duckdb -noheader -list -c "
    WITH f AS (SELECT id, name, kind, lat, lon
               FROM read_parquet('$STORE_URL/features/data.parquet') WHERE $where
               QUALIFY row_number() OVER (
                 PARTITION BY name, round(lat,2), round(lon,2) ORDER BY id) = 1)
    SELECT to_json({'type':'FeatureCollection','features': list({
      'type':'Feature',
      'properties': {'id': replace(id,':','_'), 'fid': id, 'name': name, 'kind': kind},
      'geometry': {'type':'Point','coordinates':[lon,lat]}})}) FROM f;"
}

case "${1:-}" in
  aoi) shift; aoi "$@";;
  run) shift; exec ./box.sh launch --mode both "$@";;
  all) shift; exec ./box.sh all --mode both "$@";;
  status) exec ./box.sh status;;
  "") echo "usage: $0 {aoi QUERY | run --aoi F --start S --end E | all ... | <box.sh command>}" >&2; exit 1;;
  *) exec ./box.sh "$@";;
esac
