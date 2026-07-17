#!/usr/bin/env bash
# central data desk bulk emissions detection: one cloudferro fleet, three
# detectors — flares (s2-flares SWIR flaring), mars (MARS-S2L sentinel-2
# methane ML), hypergas (EMIT hyperspectral methane) — plus ch4id attribution.
# targeting comes from the ch4id features catalogue on the shared store.
#
#   ./emissions.sh aoi 'kind=lng_terminal,status=operating' > lng.geojson
#   ./emissions.sh aoi 'GEM:G100002054200,GEM:G100002056000' > two.geojson
#   ./emissions.sh run [-d mars,flares,hypergas] --aoi lng.geojson --start 2026-06-01 --end 2026-07-17
#   ./emissions.sh status | pull | archive [-d …] | attribute [n]
#   ./emissions.sh cron 'kind=lng_terminal,status=operating' [dets] [lookback-days]
#   anything else (up, down, cost, ssh, publish, watch, image …) → box.sh
#
# fleet primitives come from box.sh (sourced); detector specifics live in
# detectors/<name>.sh — each defines <d>_repo (dir on the box), <d>_prep i
# (push + deps), <d>_cmd (run script, expanded box-side against
# START/END/SHARD/NSHARDS/AOI/SITES), <d>_merge (head-side archive script,
# env AK/SK/REGION/BUCKET/RUN) and <d>_pull i. adding a detector = one file.
set -euo pipefail
CALLER="$PWD"; cd "$(dirname "$0")"
. ./box.sh                            # auth, up, mssh, mip, fleetn, shard_aoi, s3creds, ARCHIVER, store.sh (note: resets SELF_PWD)
: "${MARS_DIR:=$HOME/Research/mars-s2l}"
: "${HYPERGAS_DIR:=$HOME/Tools/hypergas}"
: "${CH4ID_DIR:=$HOME/Tools/ch4id}"

# ch4id is a private repo — push the working tree like the other payloads
# (durable data comes from the store, so only the code travels)
ch4id_push(){
  rsync -az -e "ssh $SSHOPTS -i $KEYFILE" --delete \
    --exclude .work --exclude data --exclude .git --exclude .env \
    "$CH4ID_DIR/" "eouser@$(mip "$1"):ch4id/"
}
DETS_ALL="flares,mars,hypergas"
for f in detectors/*.sh; do . "$f"; done

# ── targeting: the ch4id features catalogue (public, on the store) → aoi geojson.
# QUERY is either comma-separated k=v filters (kind/status/dataset/fuel/…) or a
# comma-separated list of feature ids (GEM:…, OGIM:…, OSM:…). one point feature
# per site, deduped across co-located units; `id` is filesystem-safe, `fid` raw.
aoi(){
  local q=${1:?aoi QUERY: 'kind=lng_terminal[,status=operating]' or 'GEM:id[,id…]'} where
  if [[ $q == *=* ]]; then where="$(sed "s/=/ = '/g; s/,/' AND /g" <<<"$q")'"
  else where="id IN ('$(sed "s/,/','/g" <<<"$q")')"; fi
  duckdb -noheader -list -c "
    WITH f AS (SELECT id, name, kind, lat, lon
               FROM read_parquet('$STORE_URL/features/data.parquet') WHERE $where
               QUALIFY row_number() OVER (PARTITION BY name, round(lat,2), round(lon,2) ORDER BY id) = 1)
    SELECT to_json({'type':'FeatureCollection','features': list({
      'type':'Feature',
      'properties': {'id': replace(id,':','_'), 'fid': id, 'name': name, 'kind': kind},
      'geometry': {'type':'Point','coordinates':[lon,lat]}})})
    FROM f;"
}

# aoi geojson → the sites csv the methane detectors take (centroids)
sites_csv(){ python3 - "$1" <<'PY'
import json, re, sys
print("location_name,lon,lat")
for i, f in enumerate(json.load(open(sys.argv[1]))["features"]):
    g = f["geometry"]; c = g["coordinates"]
    if g["type"] != "Point":
        ring = c[0] if g["type"] == "Polygon" else c
        c = [sum(p[0] for p in ring)/len(ring), sum(p[1] for p in ring)/len(ring)]
    p = f.get("properties") or {}
    name = re.sub(r"[^\w.-]", "_", str(p.get("id") or p.get("name") or i))
    print(f"{name},{c[0]},{c[1]}")
PY
}

# ── head-side merge template for the csv-per-shard methane detectors: union the
# box's out/results_*.csv with the live store object (best status wins per
# scene; a fresh local row supersedes an archived one — reruns are corrections),
# stage locally, replace — sequential writes into the live archive.
results_merge(){ local repo=$1 prefix=$2 key=$3; cat <<EOS
set -euo pipefail; cd \$HOME/$repo
ls out/results_*.csv >/dev/null 2>&1 || { echo "$prefix: no results — skipping"; exit 0; }
S3="INSTALL httpfs; LOAD httpfs; SET s3_endpoint='s3.\$REGION.cloudferro.com'; SET s3_region='\$REGION'; SET s3_url_style='path'; SET s3_access_key_id='\$AK'; SET s3_secret_access_key='\$SK';"
new="SELECT *, '\${RUN:-bulk}' AS run, 0 AS _pri FROM read_csv('out/results_*.csv', union_by_name=true)"
duckdb -c "\$S3 SELECT 1 FROM read_parquet('s3://\$BUCKET/$prefix/results.parquet') LIMIT 1" >/dev/null 2>&1 \
  && new="\$new UNION BY NAME SELECT *, 1 AS _pri FROM read_parquet('s3://\$BUCKET/$prefix/results.parquet')"
duckdb -c "\$S3 COPY (SELECT * EXCLUDE(_pri) FROM (\$new)
    QUALIFY row_number() OVER (PARTITION BY $key
      ORDER BY CASE WHEN status LIKE 'error%' THEN 2 WHEN status = 'ok' THEN 0 ELSE 1 END, _pri) = 1)
  TO '/tmp/$repo-results.parquet' (FORMAT parquet);
  COPY (SELECT * FROM read_parquet('/tmp/$repo-results.parquet'))
  TO 's3://\$BUCKET/$prefix/results.parquet' (FORMAT parquet);"
[ -d out/plumes ] && AWS_ACCESS_KEY_ID=\$AK AWS_SECRET_ACCESS_KEY=\$SK \
  aws --endpoint-url "https://s3.\$REGION.cloudferro.com" s3 sync out/plumes "s3://\$BUCKET/$prefix/plumes/" --size-only
true
EOS
}

run(){
  local dets=$DETS_ALL aoi="" f="" c i n d
  START="" END="" BUFFER=2
  while [ $# -gt 0 ]; do case $1 in
    -d) dets=$2; shift 2;; --aoi) aoi=$2; shift 2;; --start) START=$2; shift 2;;
    --end) END=$2; shift 2;; --buffer) BUFFER=$2; shift 2;;
    *) echo "run: unknown arg $1" >&2; exit 1;;
  esac; done
  : "${aoi:?--aoi required}" "${START:?--start required}" "${END:?--end required}"
  for c in "$aoi" "$CALLER/$aoi" "../$aoi"; do [ -f "$c" ] && { f=$c; break; }; done
  [ -n "$f" ] || { echo "aoi file not found: $aoi" >&2; exit 1; }
  export START END BUFFER
  up
  n=$FLEET; echo "$n" > .fleet
  shard_aoi "$f" "$n"
  sites_csv "$f" > /tmp/_sites.csv
  for i in $(seq 0 $((n-1))); do prep_member "$i" "$dets" & done; wait
  for i in $(seq 0 $((n-1))); do launch_member "$i" "$n" "$dets"; done
  say "fleet detached & resumable — ./emissions.sh status"
}

prep_member(){
  local i=$1 dets=$2 d ip; ip=$(mip "$i")
  say "  [$i] waiting for cloud-init…"
  wait_ready "$i" || { echo "  [$i] not ready after ~20min — ./emissions.sh ssh $i" >&2; return 1; }
  scp -q $SSHOPTS -i "$KEYFILE" "/tmp/_shard-$i.geojson" "eouser@$ip:_aoi.geojson"
  scp -q $SSHOPTS -i "$KEYFILE" /tmp/_sites.csv "eouser@$ip:_sites.csv"
  for d in ${dets//,/ }; do "${d}_prep" "$i"; done
  say "  [$i] ready"
}

# one detached runner per box: detectors run sequentially (they share the cpu),
# each in its own subshell so one failing doesn't stop the next.
launch_member(){
  local i=$1 n=$2 dets=$3 d f="/tmp/_run-$i.sh"
  { for d in ${dets//,/ }; do echo "( $("${d}_cmd") ) || echo '$d failed' >&2"; done; } > "$f"
  scp -q $SSHOPTS -i "$KEYFILE" "$f" "eouser@$(mip "$i"):_run.sh"
  mssh "$i" "pkill -x s2-flares 2>/dev/null; pkill -f '[s]rc.monitor|[b]ulk.py' 2>/dev/null; sleep 1
    nohup env START='$START' END='$END' BUFFER='$BUFFER' SHARD=$i NSHARDS=$n \
      bash _run.sh > emissions.log 2>&1 < /dev/null & disown
    echo \"  [$i] detached: $dets\""
}

status(){
  local n i d; n=$(fleetn)
  for i in $(seq 0 $((n-1))); do
    echo "── box $i ──"
    mssh "$i" 'pgrep -af "[s]2-flares detect|[s]rc.monitor|[b]ulk.py" | sed "s/^/  running: /"
      tail -2 emissions.log 2>/dev/null | sed "s/^/  /"' || true
    for d in ${DETS_ALL//,/ }; do "${d}_count" "$i" || true; done
  done
}

pull(){
  local n i d; n=$(fleetn)
  for d in ${DETS_ALL//,/ }; do for i in $(seq 0 $((n-1))); do "${d}_pull" "$i" 2>/dev/null || true; done; done
  say "pulled"
}

# gather every member's outputs onto the head, then run each detector's merge
# there with store creds. RUN tags the rows (bulk/replication/cron).
archive(){
  local dets=$DETS_ALL; [ "${1:-}" = -d ] && dets=$2
  auth; local ak sk; read -r ak sk < <(s3creds)
  openstack container show "$BUCKET" >/dev/null 2>&1 || openstack container create "$BUCKET" >/dev/null
  local n d i repo; n=$(fleetn)
  for d in ${dets//,/ }; do
    repo=$("${d}_repo")
    for ((i=1; i<n; i++)); do
      mssh "$i" "cd $repo 2>/dev/null && tar cf - out 2>/dev/null || true" \
        | mssh 0 "mkdir -p $repo && tar xf - -C $repo"
    done
    say "archive $d → store"
    "${d}_merge" | mssh 0 "AK='$ak' SK='$sk' REGION='$OS_REGION_NAME' BUCKET='$BUCKET' RUN='${RUN:-bulk}' bash -s"
  done
}

# ch4id attribution of unattributed datadesk plumes, on the head box.
attribute(){
  local nn=${1:-20}
  auth; local ak sk; read -r ak sk < <(s3creds)
  wait_ready 0
  ch4id_push 0
  env | grep -E '^(DEEPSEEK|OPENROUTER)_API_KEY=' | mssh 0 'umask 077; cat > .ch4id.env' || true
  mssh 0 "cd ch4id && AK='$ak' SK='$sk' REGION='$OS_REGION_NAME' BUCKET='$BUCKET' bin/box run -n $nn"
}

# standing daily incremental run on the head box: detect new scenes over the
# standing aoi → merge into the live archive → attribute. store creds + llm
# keys are persisted box-side once, here.
cron(){
  local q=${1:?aoi query} dets=${2:-$DETS_ALL} lb=${3:-14} d ip
  auth; local ak sk; read -r ak sk < <(s3creds)
  wait_ready 0; ip=$(mip 0)
  aoi "$q" > /tmp/_cron_aoi.geojson
  sites_csv /tmp/_cron_aoi.geojson > /tmp/_cron_sites.csv
  scp -q $SSHOPTS -i "$KEYFILE" /tmp/_cron_aoi.geojson "eouser@$ip:_cron_aoi.geojson"
  scp -q $SSHOPTS -i "$KEYFILE" /tmp/_cron_sites.csv "eouser@$ip:_cron_sites.csv"
  scp -q $SSHOPTS -i "$KEYFILE" daily.sh "eouser@$ip:daily.sh"
  for d in ${dets//,/ }; do
    "${d}_prep" 0
    { echo "set -e"; "${d}_cmd"; } > "/tmp/_run-$d.sh"
    "${d}_merge" > "/tmp/_merge-$d.sh"
    scp -q $SSHOPTS -i "$KEYFILE" "/tmp/_run-$d.sh" "eouser@$ip:run-$d.sh"
    scp -q $SSHOPTS -i "$KEYFILE" "/tmp/_merge-$d.sh" "eouser@$ip:merge-$d.sh"
  done
  ch4id_push 0
  { printf 'AK=%s\nSK=%s\nREGION=%s\nBUCKET=%s\nDETS=%s\nLOOKBACK=%s\n' \
      "$ak" "$sk" "$OS_REGION_NAME" "$BUCKET" "$dets" "$lb"
    env | grep -E '^(DEEPSEEK|OPENROUTER)_API_KEY=' || true
  } | mssh 0 'umask 077; cat > .emissions.env'
  mssh 0 '(crontab -l 2>/dev/null | grep -v daily.sh; echo "47 2 * * * bash $HOME/daily.sh >> $HOME/daily.log 2>&1") | crontab -'
  say "cron installed on head ($ip): daily 02:47 UTC, dets=$dets, lookback=${lb}d — tail: ./emissions.sh ssh 0 then tail -f daily.log"
}

case "${1:-}" in
  aoi) shift; aoi "$@";;
  run) shift; run "$@";;
  status) status;;
  pull) pull;;
  archive) shift; archive "$@";;
  attribute) shift; attribute "${1:-}";;
  cron) shift; cron "$@";;
  "") echo "usage: $0 {aoi QUERY | run [-d d1,d2] --aoi F --start S --end E | status | pull | archive [-d …] | attribute [n] | cron QUERY [dets] [days] | <box.sh subcommand>}" >&2; exit 1;;
  *) exec ./box.sh "$@";;
esac
