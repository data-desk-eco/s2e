#!/usr/bin/env bash
# CloudFerro WAW3-2 fleet orchestration — see cloud/README.md for the subcommand
# reference, auth (.env), fleet model, and the published archive layout.
set -euo pipefail
SELF_PWD="$PWD"             # caller's cwd — to resolve relative --aoi paths after the cd below
cd "$(dirname "$0")"

# GPU=1 → the full-tile nvJPEG2000 path (gpu cloud-init + L40S flavor + NVIDIA image).
if [ "${GPU:-}" = 1 ]; then
  : "${CLOUD_INIT:=cloud-init-gpu.yaml}"; : "${FLAVOR:=vm.l40s.1}"
  : "${IMAGE:=Ubuntu 22.04 NVIDIA}"; : "${RATE:=0.45}"; : "${FLEET:=1}"
fi
: "${CLOUD_INIT:=cloud-init.yaml}"
: "${VM:=s2-flares}"            # base name; fleet members are $VM-0 … $VM-(FLEET-1)
: "${FLEET:=4}"                 # fleet size for bulk (--aoi) runs; a bbox run forces 1
: "${FLAVOR:=eo1.large}"
: "${BASEOS:=Ubuntu 22.04 LTS}"   # stock distro for a cold build / for baking the golden image
: "${BASEIMG:=s2-flares-base}"    # golden snapshot baked by `image`; auto-booted from if it exists
: "${IMAGE:=}"                    # boot image; empty → resolve_image() picks $BASEIMG or $BASEOS
: "${KEYPAIR:=s2-flares}"
: "${KEYFILE:=$HOME/.ssh/id_ed25519}"
: "${SECGROUP:=s2-flares}"
: "${NET:=s2-flares-net}"
: "${EXTNET:=external}"
: "${EODATANET:=eodata}"
: "${REPO_DIR:=s2-flares}"      # repo path on the box
: "${OUT:=out}"                 # box-side output dir (s2-flares detect writes <OUT>/<id>/<mgrs>_<date>.csv)
: "${LOCAL_DATA:=../data/cf}"   # where `pull` lands the CSVs
: "${BUCKET:=s2-flares-archive}"  # CloudFerro object-storage container for `archive`
: "${RATE:=0.066}"              # eo1.large pay-per-use €/h (WAW3-2); override per flavor

SSHOPTS="-o LogLevel=ERROR -o StrictHostKeyChecking=accept-new -o UserKnownHostsFile=/dev/null"
say(){ printf '\033[1;36m→ %s\033[0m\n' "$*"; }
# fleet members are $VM-0 … $VM-(FLEET-1), each with its own floating IP in .box-ip-<i>.
# `run` pins the active member count in .fleet (a bbox run writes 1); every op that
# post-dates it reads .fleet so the fan-out matches exactly the boxes that ran.
mvm(){ echo "$VM-$1"; }
mip(){ cat ".box-ip-$1" 2>/dev/null || { echo "no box $VM-$1 — run ./box.sh up" >&2; exit 1; }; }
mssh(){ local i=$1; shift; ssh $SSHOPTS -i "$KEYFILE" "eouser@$(mip "$i")" "$@"; }
fleetn(){ cat .fleet 2>/dev/null || echo "$FLEET"; }
boxip(){ mip "${1:-0}"; }       # head IP
sshx(){ mssh 0 "$@"; }          # single-box convenience (parity / head ops)
# project S3 (EC2) creds for our own buckets — list, minting one if none. "ak sk".
s3creds(){
  local c; c=$(openstack ec2 credentials list -f json | jq -r '.[0]|"\(.Access) \(.Secret)"' 2>/dev/null || true)
  { [ -z "${c:-}" ] || [ "$c" = "null null" ]; } && { openstack ec2 credentials create >/dev/null; c=$(openstack ec2 credentials list -f json | jq -r '.[0]|"\(.Access) \(.Secret)"'); }
  echo "$c"
}

auth(){
  [ -n "${OS_TOKEN:-}" ] && return 0   # reuse the session within one invocation (one TOTP use)
  [ -f ../.env ] && . ../.env; [ -f .env ] && . .env
  set +eu   # the vendored openrc is written for a lax shell (unset OS_* refs, own `return`s)
  if [ -n "${CLOUDFERRO_TOTP_SECRET:-}" ] && command -v oathtool >/dev/null; then
    source ./s2-flares-openrc-2fa.sh >/dev/null \
      < <(printf '%s\n%s\n' "${CLOUDFERRO_PASSWORD:-}" "$(oathtool -b --totp "$CLOUDFERRO_TOTP_SECRET")")
  else
    source ./s2-flares-openrc-2fa.sh
  fi
  set -eu
  unset IFS   # the openrc leaves IFS=$'\n'; restore default splitting (else `ssh $SSHOPTS` collapses to one arg)
  [ -n "${OS_TOKEN:-}" ] || { echo "auth failed: no keystone token — wrong password/TOTP (check .env), or token-issue rejected" >&2; exit 1; }
}

# idempotently ensure shared infra: keypair, ssh-only secgroup, private net + router.
infra(){
  say "Keypair $KEYPAIR"
  [ -f "$KEYFILE" ] || ssh-keygen -t ed25519 -N '' -f "$KEYFILE" >/dev/null
  openstack keypair show "$KEYPAIR" >/dev/null 2>&1 || openstack keypair create --public-key "$KEYFILE.pub" "$KEYPAIR" >/dev/null
  say "Security group $SECGROUP"
  openstack security group show "$SECGROUP" >/dev/null 2>&1 || {
    openstack security group create "$SECGROUP" >/dev/null
    openstack security group rule create --proto tcp --dst-port 22 --remote-ip 0.0.0.0/0 "$SECGROUP" >/dev/null; }
  say "Network $NET"
  openstack network show "$NET" >/dev/null 2>&1 || {
    openstack network create "$NET" >/dev/null
    openstack subnet create --network "$NET" --subnet-range 10.0.42.0/24 --dns-nameserver 8.8.8.8 "$NET-sub" >/dev/null
    openstack router create "$NET-rtr" >/dev/null
    openstack router set --external-gateway "$EXTNET" "$NET-rtr" >/dev/null
    openstack router add subnet "$NET-rtr" "$NET-sub" >/dev/null; }
}
# pick the boot image: explicit IMAGE wins; else the golden $BASEIMG snapshot if baked
# (warm <1min boot); else the stock distro (cold build). needs auth (a glance lookup).
resolve_image(){
  [ -n "$IMAGE" ] && return
  if openstack image show "$BASEIMG" >/dev/null 2>&1; then IMAGE=$BASEIMG; say "Image $BASEIMG (golden → fast boot)"
  else IMAGE=$BASEOS; say "Image $BASEOS (no golden image — cold build; bake one with ./box.sh image)"; fi
}

# provision shared infra, then boot the FLEET in parallel (idempotent; reuses members).
up(){
  auth; infra; resolve_image
  local netid eoid; netid=$(openstack network show "$NET" -f value -c id); eoid=$(openstack network show "$EODATANET" -f value -c id)
  say "Booting fleet of $FLEET ($FLAVOR) in parallel — a few minutes…"
  local i; for i in $(seq 0 $((FLEET-1))); do boot_member "$i" "$netid" "$eoid" & done; wait
  printf '\n\033[1;32m✓ provisioned %s members\033[0m  →  ./box.sh ssh [i]\n' "$FLEET"
}

# bake/refresh the golden image: boot one stock box → full cold build → strip per-VM
# creds + cloud-init state → snapshot to $BASEIMG → tear down. (see README)
image(){
  auth; infra
  local netid eoid; netid=$(openstack network show "$NET" -f value -c id); eoid=$(openstack network show "$EODATANET" -f value -c id)
  IMAGE=$BASEOS
  say "Baking $BASEIMG from $BASEOS — full cold install+build, ~8min…"
  boot_member img "$netid" "$eoid"
  wait_ready img || { echo "cloud-init didn't finish on $(mvm img) — ./box.sh ssh img" >&2; return 1; }
  say "Stripping per-VM creds + cloud-init state, then snapshotting → $BASEIMG"
  mssh img 'sudo rm -f /etc/profile.d/eodata.sh && sudo cloud-init clean --logs'
  openstack image delete "$BASEIMG" >/dev/null 2>&1 || true   # replace any prior bake
  openstack server image create --name "$BASEIMG" --wait "$(mvm img)" >/dev/null
  down_member img; rm -f .box-ip-img
  printf '\n\033[1;32m✓ baked %s — every ./box.sh up now boots from it\033[0m\n' "$BASEIMG"
}
# one member: boot (idempotent) + attach a floating IP → .box-ip-<i>. run under `&`.
boot_member(){
  local i=$1 netid=$2 eoid=$3 vm; vm=$(mvm "$i")
  openstack server show "$vm" >/dev/null 2>&1 || openstack server create "$vm" \
    --flavor "$FLAVOR" --image "$IMAGE" --key-name "$KEYPAIR" --security-group "$SECGROUP" \
    --nic net-id="$netid" --nic net-id="$eoid" --user-data "$CLOUD_INIT" --wait >/dev/null
  local port fip
  port=$(openstack port list --server "$vm" --network "$NET" -f value -c id | head -1)
  fip=$(openstack floating ip list --port "$port" -f value -c "Floating IP Address" | head -1)
  [ -n "$fip" ] || { fip=$(openstack floating ip create "$EXTNET" -f value -c floating_ip_address); openstack floating ip set --port "$port" "$fip" >/dev/null; }
  echo "$fip" > ".box-ip-$i"
  say "  [$i] $vm @ $fip"
}

ip(){
  auth; local i port
  for i in $(seq 0 $(($(fleetn)-1))); do
    port=$(openstack port list --server "$(mvm "$i")" --network "$NET" -f value -c id | head -1)
    openstack floating ip list --port "$port" -f value -c "Floating IP Address" | head -1 | tee ".box-ip-$i"
  done
}

# interactive login to member ${1:-0}. `exec` so ssh doesn't slurp our stdin.
go_ssh(){ exec ssh $SSHOPTS -i "$KEYFILE" "eouser@$(mip "${1:-0}")"; }

# shard the --aoi round-robin across the fleet, scp each shard, rebuild, launch the
# detached resumable detect per member in parallel. bbox/no-aoi → 1 member. .fleet
# pins how many ran so later ops fan out to match. (see README)
run(){
  local a=("$@") aoi="" rest=() i f
  for ((i=0; i<${#a[@]}; i++)); do
    if [ "${a[i]}" = "--aoi" ]; then
      # resolve --aoi against caller cwd + repo root; resolving NOWHERE is a hard error
      # (never a silent fall-through to a 1-box bbox run that collapses a sharded fleet).
      f="${a[i+1]:-}"
      for c in "$f" "$SELF_PWD/$f" "$PWD/../$f"; do [ -f "$c" ] && { aoi="$c"; break; }; done
      [ -n "$aoi" ] || { echo "--aoi file not found: $f (looked in caller cwd + repo root)" >&2; exit 1; }
      i=$((i+1)); continue   # i++ returns 0 at i=0 → set -e abort on old bash
    fi
    rest+=("${a[i]}")
  done
  local n=$FLEET; [ -n "$aoi" ] || n=1
  echo "$n" > .fleet
  [ -n "$aoi" ] && { say "Sharding $aoi across $n members"; shard_aoi "$aoi" "$n"; }
  local feat="" cudaenv=""
  if [ "${GPU:-}" = 1 ]; then feat=" --features gpu"; cudaenv=". /etc/profile.d/cuda.sh && "
    [[ " ${rest[*]} " == *" --gpu "* ]] || rest+=(--gpu); fi
  for i in $(seq 0 $((n-1))); do start_member "$i" "$aoi" "$feat" "$cudaenv" "${rest[@]}" & done; wait
  say "Fleet detached & resumable — streaming progress (Ctrl-C is safe, the runs continue)"
  watch
}
# poll until cloud-init produced a working toolchain + built tree (rides out sshd-not-
# yet-up: mssh fails → retry). cold box ~5-8min; golden boot near-instant. capped ~20min.
wait_ready(){
  local i=$1 w=0
  until mssh "$i" 'test -f "$HOME/.cargo/env" && test -x '"$REPO_DIR"'/target/release/s2-flares' 2>/dev/null; do
    w=$((w+1)); [ "$w" -gt 120 ] && return 1; sleep 10
  done
}
start_member(){
  local i=$1 aoi=$2 feat=$3 cudaenv=$4; shift 4; local rest=("$@") ip aoiarg=""; ip=$(mip "$i")
  say "  [$i] waiting for cloud-init (rust/gdal/first build)…"
  wait_ready "$i" || { echo "  [$i] not ready after ~20min — check ./box.sh ssh $i" >&2; return 1; }
  if [ -n "$aoi" ]; then
    scp -q $SSHOPTS -i "$KEYFILE" "/tmp/_shard-$i.geojson" "eouser@$ip:$REPO_DIR/_aoi.geojson"
    aoiarg="--aoi _aoi.geojson"; fi
  say "  [$i] build (incremental)$feat"
  mssh "$i" "cd $REPO_DIR && git pull -q && . \$HOME/.cargo/env && ${cudaenv}cargo build --release -q -p s2-flares-cli$feat"
  # stop any running detect first (re-run is idempotent/resumable — no racing detects).
  mssh "$i" "pkill -x s2-flares 2>/dev/null; sleep 1; cd $REPO_DIR && nohup bash -lc './target/release/s2-flares detect --source cdse $aoiarg ${rest[*]} --out $OUT' >/tmp/cfrun.log 2>&1 & sleep 1; echo '  [$i] detect started'"
}
# round-robin split → /tmp/_shard-<i>.geojson (balanced slices; keeps the cli generic).
shard_aoi(){ python3 - "$1" "$2" <<'PY'
import json,sys
fs=json.load(open(sys.argv[1]))['features']; n=int(sys.argv[2])
for i in range(n): json.dump({'type':'FeatureCollection','features':fs[i::n]}, open(f'/tmp/_shard-{i}.geojson','w'))
PY
}

# stream every member's detect log (prefixed [i]) until all printed `done:` or crashed
# (detect vanished after output). unbounded — a wide run outlasts any cap. one ssh/member/
# cycle returns the new slice + a 0x1e-prefixed `<lines> <state>` tag (RS never in a log
# line, so the split is unambiguous).
watch(){
  local n i; n=$(fleetn); local -a seen
  for i in $(seq 0 $((n-1))); do seen[i]=0; done
  while :; do
    local fin=0
    for i in $(seq 0 $((n-1))); do
      local out; out=$(mssh "$i" "l=/tmp/cfrun.log; t=\$(wc -l <\$l 2>/dev/null||echo 0); sed -n \"$((seen[i]+1)),\${t}p\" \$l 2>/dev/null; printf '\036%s %s' \$t \"\$(grep -q '^done:' \$l 2>/dev/null && echo D || { pgrep -x s2-flares >/dev/null 2>&1 || echo X; })\"" 2>/dev/null) || { fin=$((fin+1)); continue; }
      local body=${out%$'\036'*} t st; read -r t st <<<"${out##*$'\036'}"
      [[ "$t" =~ ^[0-9]+$ ]] || t=${seen[i]}
      [ -n "$body" ] && printf '%s\n' "$body" | sed "s/^/[$i] /"
      seen[i]=$t
      { [ "$st" = D ] || { [ "$st" = X ] && [ "$t" -gt 0 ]; }; } && fin=$((fin+1))
    done
    [ "$fin" -ge "$n" ] && break
    sleep 3
  done
}

# gpu↔cpu parity gate (gpu box only): nvJPEG2000 == GDAL/OpenJPEG detections byte-for-
# byte over real scenes. set PARITY_BBOX (small); narrow with PARITY_TILE/START/END.
parity(){
  : "${PARITY_BBOX:?set PARITY_BBOX=W,S,E,N (a small test region)}"
  say "Parity gpu-vs-cpu on head"
  sshx "cd $REPO_DIR && git pull -q && . \$HOME/.cargo/env && . /etc/profile.d/cuda.sh && . /etc/profile.d/eodata.sh && \
    S2_PARITY_BBOX='$PARITY_BBOX' ${PARITY_TILE:+S2_PARITY_TILE='$PARITY_TILE'} ${START:+S2_PARITY_START='$START'} ${END:+S2_PARITY_END='$END'} \
    cargo test --release -p s2-flares-cli --features gpu parity -- --ignored --nocapture"
}

pull(){
  local n i; n=$(fleetn); mkdir -p "$LOCAL_DATA"
  for i in $(seq 0 $((n-1))); do
    say "Pull $(mvm "$i"):$OUT → $LOCAL_DATA"
    rsync -az -e "ssh $SSHOPTS -i $KEYFILE" "eouser@$(mip "$i"):$REPO_DIR/$OUT/" "$LOCAL_DATA/" &
  done; wait
  echo "  $(find "$LOCAL_DATA" -name '*.csv' | wc -l | tr -d ' ') scene CSVs in $LOCAL_DATA"
}

# roll the per-scene CSVs up into the published artifacts (detections/, clouds/,
# clusters/) in one head-side pass, then refresh coverage.geojson. each worker holds
# only its shard, so we GATHER every member's files onto the head first — one rollup
# there sees the whole archive (clean per-tile DISTINCT, complete cloud mask). duckdb
# (rollup) + the cli (cluster) then run on the head. see cloud/README.md for the layout.
archive(){
  auth
  openstack container show "$BUCKET" >/dev/null 2>&1 || { say "Bucket $BUCKET"; openstack container create "$BUCKET" >/dev/null; }
  local ak sk; read -r ak sk < <(s3creds)
  local n i; n=$(fleetn)
  # gather workers' CSVs → head (tar piped via local host — workers can't ssh each
  # other; CSVs are tiny). distinct <id>/ dirs → no collision.
  for ((i=1; i<n; i++)); do
    say "Gather $(mvm "$i") CSVs → head"
    mssh "$i" "cd $REPO_DIR/$OUT 2>/dev/null && tar cf - . || true" | mssh 0 "mkdir -p $REPO_DIR/$OUT && tar xf - -C $REPO_DIR/$OUT"
  done
  say "Archive $OUT → s3://$BUCKET/{detections (per-tile parquet),clouds (cloud mask)}"
  local b64; b64=$(printf '%s' "$ARCHIVER" | base64 | tr -d '\n')
  mssh 0 "echo $b64 | base64 -d | AK='$ak' SK='$sk' REGION='$OS_REGION_NAME' BUCKET='$BUCKET' OUT='$REPO_DIR/$OUT' bash"
  say "Cluster view (+ clear-sky persistence, joined against clouds/) → s3://$BUCKET/clusters/mgrs=…/"
  # persistence folds in via the cloud mask (anchor's ~100 m cell ⋈ clouds/) — pure
  # duckdb, no 2nd SCL pass. persistence window == detection window (START/END drive both).
  mssh 0 "cd $REPO_DIR && \
        S2_S3_ENDPOINT='s3.$OS_REGION_NAME.cloudferro.com' S2_S3_REGION='$OS_REGION_NAME' \
        S2_S3_ACCESS_KEY='$ak' S2_S3_SECRET_KEY='$sk' \
        ./target/release/s2-flares cluster --concurrency ${COVERAGE_CONCURRENCY:-16} \
          --archive 's3://$BUCKET/detections/**/*.parquet' --clouds 's3://$BUCKET/clouds/data.parquet' \
          --out 's3://$BUCKET/clusters' --start '${START:-2015-01-01}' --end '${END:-2100-01-01}'"
  coverage || true   # refresh the scanned-extent overlay from the live shards (best-effort)
}

# (re)build s3://$BUCKET/coverage.geojson — one Polygon per scanned AOI feature, the
# web map's coverage overlay + archive-vs-detect test. merges INTO the existing object
# by feature id (re-scan replaces, new AOIs append, no dupes) → coverage grows
# monotonically across runs. features come from a local AOI file ($1) or, by default,
# the union of the live fleet's shards. stamped {name, start, end, scanned}. needs aws.
coverage(){
  command -v aws >/dev/null || { say "coverage needs aws-cli — skipping coverage.geojson"; return 0; }
  auth; local ak sk; read -r ak sk < <(s3creds)
  local tmp; tmp=$(mktemp -d); trap "rm -rf '$tmp'" RETURN
  if [ -n "${1:-}" ]; then
    local src=""; for c in "$1" "$SELF_PWD/$1" "$PWD/../$1"; do [ -f "$c" ] && { src="$c"; break; }; done
    [ -n "$src" ] || { echo "coverage: AOI file not found: $1" >&2; return 1; }
    cp "$src" "$tmp/aoi-0.json"
  else local n i; n=$(fleetn); for i in $(seq 0 $((n-1))); do mssh "$i" "cat $REPO_DIR/_aoi.geojson 2>/dev/null" > "$tmp/aoi-$i.json" || true; done; fi
  local s3=(env AWS_ACCESS_KEY_ID="$ak" AWS_SECRET_ACCESS_KEY="$sk" AWS_DEFAULT_REGION="$OS_REGION_NAME"
    aws --endpoint-url "https://s3.$OS_REGION_NAME.cloudferro.com" --no-cli-pager s3)
  "${s3[@]}" cp "s3://$BUCKET/coverage.geojson" "$tmp/cur.json" 2>/dev/null || echo '{"type":"FeatureCollection","features":[]}' > "$tmp/cur.json"
  START="${START:-2015-01-01}" END="${END:-2100-01-01}" python3 - "$tmp" <<'PY' || { say "coverage: no AOI features (bbox run?) — skipped"; return 0; }
import json,os,glob,sys,datetime
tmp=sys.argv[1]
cov=json.load(open(f'{tmp}/cur.json')); feats={f['properties']['id']:f for f in cov.get('features',[]) if f.get('properties',{}).get('id')}
start,end,scanned=os.environ['START'],os.environ['END'],os.environ.get('SCANNED') or datetime.date.today().isoformat()
n=0
for af in glob.glob(f'{tmp}/aoi-*.json'):
    try: src=json.load(open(af))
    except Exception: continue
    for i,f in enumerate(src.get('features',[])):
        p=f.get('properties') or {}
        fid=next((p[k] for k in ('id','ProjectID') if isinstance(p.get(k),str)), None)
        if not (fid and f.get('geometry')): continue
        feats[fid]={'type':'Feature','geometry':f['geometry'],
                    'properties':{'id':fid,'name':p.get('name',''),'start':start,'end':end,'scanned':scanned}}; n+=1
if not n: sys.exit(1)
json.dump({'type':'FeatureCollection','features':list(feats.values())}, open(f'{tmp}/new.json','w'))
print(f'  coverage.geojson: {n} scanned features merged → {len(feats)} total')
PY
  "${s3[@]}" cp "$tmp/new.json" "s3://$BUCKET/coverage.geojson"
}

# runs on the head: one COPY per tile (union of all that tile's non-empty scene CSVs)
# to its deterministic S3 key, then a read-back tally. heredoc'd so box.sh stays one file.
ARCHIVER=$(cat <<'EOS'
set -euo pipefail
DDB=~/.duckdb/cli/latest/duckdb
S3="INSTALL httpfs; LOAD httpfs;
SET s3_endpoint='s3.$REGION.cloudferro.com'; SET s3_region='$REGION';
SET s3_url_style='path'; SET s3_use_ssl=true;
SET s3_access_key_id='$AK'; SET s3_secret_access_key='$SK';"
# tiles with ≥1 detection (skip header-only scenes); <mgrs>_<date>.csv → mgrs.
tiles=$(for f in "$OUT"/*/*.csv; do [ "$(wc -l <"$f")" -gt 1 ] && b=$(basename "$f" .csv) && echo "${b%_*}" || :; done | sort -u)
{ echo "$S3"
  for m in $tiles; do
    echo "COPY (SELECT DISTINCT * EXCLUDE(mgrs) FROM read_csv('$OUT/*/${m}_*.csv', union_by_name=true) ORDER BY date) TO 's3://$BUCKET/detections/mgrs=$m/data.parquet' (FORMAT parquet);"
  done
  # clouds/ the cloud mask — one row per (~100 m cell, date), the persistence denominator
  # joined into clusters/ at build time. internal artifact; browser never reads it. (README)
  echo "COPY (SELECT DISTINCT glon,glat,date,cloud_frac FROM read_csv('$OUT/*/*.cld', union_by_name=true) ORDER BY date) TO 's3://$BUCKET/clouds/data.parquet' (FORMAT parquet);"
} | "$DDB"
"$DDB" -c "$S3 SELECT count(*) AS archived_rows, count(DISTINCT date) AS dates FROM read_parquet('s3://$BUCKET/detections/**/data.parquet', hive_partitioning=true);"
EOS
)

# web-map backend: anonymous public-read on detections/* + clusters/* + vnf/* +
# coverage.geojson + CORS, so a browser (DuckDB-wasm) can range-read directly. clouds/
# stays private (internal build artifact). one-time per bucket; needs aws-cli. (README)
publish(){
  auth
  command -v aws >/dev/null || { echo "publish needs aws-cli (brew install awscli)" >&2; exit 1; }
  local ak sk; read -r ak sk < <(s3creds)
  local aws_s3=(env AWS_ACCESS_KEY_ID="$ak" AWS_SECRET_ACCESS_KEY="$sk" AWS_DEFAULT_REGION="$OS_REGION_NAME"
    aws --endpoint-url "https://s3.$OS_REGION_NAME.cloudferro.com" --no-cli-pager s3api)
  say "Publishing s3://$BUCKET/{detections,clusters,vnf,coverage.geojson} for web-map access (public-read + CORS)"
  "${aws_s3[@]}" put-bucket-cors --bucket "$BUCKET" --cors-configuration '{"CORSRules":[{"AllowedOrigins":["*"],"AllowedMethods":["GET","HEAD"],"AllowedHeaders":["*"],"ExposeHeaders":["Content-Range","Content-Length","ETag","Accept-Ranges"],"MaxAgeSeconds":3600}]}'
  "${aws_s3[@]}" put-bucket-policy --bucket "$BUCKET" --policy '{"Version":"2012-10-17","Statement":[{"Sid":"PublicReadArchive","Effect":"Allow","Principal":{"AWS":["*"]},"Action":["s3:GetObject"],"Resource":["arn:aws:s3:::'"$BUCKET"'/detections/*","arn:aws:s3:::'"$BUCKET"'/clusters/*","arn:aws:s3:::'"$BUCKET"'/vnf/*","arn:aws:s3:::'"$BUCKET"'/coverage.geojson"]},{"Sid":"PublicListArchive","Effect":"Allow","Principal":{"AWS":["*"]},"Action":["s3:ListBucket"],"Resource":["arn:aws:s3:::'"$BUCKET"'"]}]}'
  echo "  public read + CORS applied. objects at https://s3.$OS_REGION_NAME.cloudferro.com/$BUCKET/{detections,clusters,coverage.geojson}…"
}

# empty the archive bucket (the bucket stays). IRREVERSIBLE — confirm by typing the
# bucket name back (FORCE=1 skips, for scripts).
wipe(){
  auth
  command -v aws >/dev/null || { echo "wipe needs aws-cli (brew install awscli)" >&2; exit 1; }
  local ak sk; read -r ak sk < <(s3creds)
  local rm=(env AWS_ACCESS_KEY_ID="$ak" AWS_SECRET_ACCESS_KEY="$sk" AWS_DEFAULT_REGION="$OS_REGION_NAME"
    aws --endpoint-url "https://s3.$OS_REGION_NAME.cloudferro.com" --no-cli-pager s3 rm "s3://$BUCKET/" --recursive)
  if [ "${FORCE:-}" != 1 ]; then
    local n; n=$("${rm[@]}" --dryrun | wc -l | tr -d ' ')
    [ "$n" = 0 ] && { say "s3://$BUCKET already empty"; return 0; }
    printf '\033[1;31m⚠ wipe %s objects from s3://%s — irreversible.\033[0m type the bucket name to confirm: ' "$n" "$BUCKET"
    local a; read -r a; [ "$a" = "$BUCKET" ] || { echo "  aborted"; return 1; }
  fi
  say "Wiping s3://$BUCKET"
  "${rm[@]}"
  echo "  bucket emptied."
}

# instant local cost estimate: FLEET × uptime × RATE (billing portal is daily, too
# coarse for a run in flight). assumes auth; `cost` wraps it.
costline(){
  local t; t=$(openstack server show "$(mvm 0)" -f value -c created 2>/dev/null) || return 1
  t=${t%Z}; t=${t%.*}; t=$(date -ju -f "%Y-%m-%dT%H:%M:%S" "$t" +%s 2>/dev/null || date -u -d "$t" +%s)
  local h n; n=$(fleetn); h=$(echo "scale=2;($(date -u +%s)-$t)/3600" | bc)
  printf '\033[1;36m→ %s× %s up %sh × €%s/h ≈ €%s\033[0m\n' "$n" "$FLAVOR" "$h" "$RATE" "$(echo "scale=2;$n*$h*$RATE"|bc)"
}
cost(){ auth; costline || echo "no fleet $VM-*" >&2; }

# delete every member VM + its floating IP, in parallel. costline first (best-effort).
down(){
  auth; costline || true
  local n i; n=$(fleetn)
  for i in $(seq 0 $((n-1))); do down_member "$i" & done; wait
  rm -f .box-ip-* .fleet
  printf '\n\033[1;32m✓ scaled to zero (%s members)\033[0m\n' "$n"
}
down_member(){
  local i=$1 vm port fip=""; vm=$(mvm "$i")
  port=$(openstack port list --server "$vm" --network "$NET" -f value -c id 2>/dev/null | head -1 || true)
  [ -n "$port" ] && fip=$(openstack floating ip list --port "$port" -f value -c "Floating IP Address" | head -1 || true)
  openstack server delete "$vm" --wait >/dev/null 2>&1 && say "  [$i] $vm deleted" || say "  [$i] no VM $vm"
  [ -n "$fip" ] && { openstack floating ip delete "$fip" >/dev/null 2>&1 || true; }
}

# prove a run is complete + clean per member before archive: (1) every AOI feature in
# the shard has an OUT/<id>/ dir (header-only even when flareless → no subdir == never
# scanned); (2) no leftover <mgrs>_<date>.err. nonzero if any gap/error; `all` gates the
# teardown on it. (see cloud/README.md)
verify(){
  local n i rc=0; n=$(fleetn)
  for i in $(seq 0 $((n-1))); do
    say "Verify $(mvm "$i")"
    mssh "$i" "cd $REPO_DIR && OUT='$OUT' python3 - <<'PY'
import json,os,glob,sys
out=os.environ['OUT']
errs=sorted(glob.glob(os.path.join(out,'*','*.err'))) if os.path.isdir(out) else []
if not os.path.exists('_aoi.geojson'):
    print(f'  no _aoi.geojson (bbox/region run) — {len(errs)} errored scenes')
    for e in errs: print('    errored:',e)
    sys.exit(1 if errs else 0)
def fid(i,f):
    p=f.get('properties',{}) or {}
    for k in ('id','ProjectID'):
        if isinstance(p.get(k),str): return p[k]
    return str(i)
ids=[fid(i,f) for i,f in enumerate(json.load(open('_aoi.geojson')).get('features',[]))]
scanned={d for d in (os.listdir(out) if os.path.isdir(out) else []) if glob.glob(os.path.join(out,d,'*.csv'))}
gaps=[i for i in ids if i not in scanned]
print(f'  {len(ids)-len(gaps)}/{len(ids)} shard features scanned, {len(gaps)} unscanned, {len(errs)} errored scenes')
for g in gaps: print('    unscanned:',g)
for e in errs: print('    errored:',e)
sys.exit(1 if (gaps or errs) else 0)
PY" || rc=1
  done
  return $rc
}

# up → run, detached, boxes left UP (the common entrypoint; archive/down later).
launch(){ up; run "$@"; }

# hands-off: up → run → verify → archive → pull → down. `down` fires only once verify
# proves every AOI feature was scanned — a gap keeps the fleet up for a resumable re-run.
all(){ up; run "$@"
  if verify; then archive; pull; down
  else say "verify found unscanned AOI features — fleet kept up. re-run (resumable), then ./box.sh archive && ./box.sh down"; fi; }

case "${1:-}" in
  up) up;; image) image;; ip) ip;; ssh) shift; go_ssh "${1:-0}";; cost) cost;; down) down;;
  run) shift; run "$@";; watch) watch;; pull) pull;; archive) archive;; verify) verify;; publish) publish;; wipe) wipe;; parity) parity;;
  coverage) shift; coverage "${1:-}";; launch) shift; launch "$@";; all) shift; all "$@";;
  *) echo "usage: $0 {up | image | run <args> | launch <args> | watch | pull | archive | coverage [aoi] | verify | publish | wipe | parity | cost | down | all <args> | ssh [i] | ip}  (FLEET=N, default 4; GPU=1 → gpu box)" >&2; exit 1;;
esac
