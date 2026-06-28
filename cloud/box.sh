#!/usr/bin/env bash
# CloudFerro WAW3-2 end-to-end orchestration, co-located with the Copernicus
# `eodata` archive (free in-region JP2 reads). One script, one auth path — and a
# FLEET by default: bulk runs fan out across FLEET (default 4) VMs in parallel, the
# AOI sharded one slice per member; a bbox/no-aoi run can't be split so it forces 1.
#
#   ./box.sh up                       provision the fleet (keypair/secgroup/net/VMs/IPs)
#   ./box.sh run <detect args>        shard the AOI, detached resumable detect on every member
#   ./box.sh pull                     rsync every member's per-scene CSVs down to $LOCAL_DATA
#   ./box.sh archive                  gather all members' CSVs to the head, roll up to
#                                     s3://$BUCKET/{detections,clusters,coverage}
#   ./box.sh verify                   prove every AOI feature scanned + 0 errored scenes (per member)
#   ./box.sh publish                  make that archive a web-map backend: anonymous
#                                     public-read + CORS, so DuckDB-wasm can read it
#   ./box.sh wipe                     empty the archive bucket (confirms; FORCE=1 skips)
#   ./box.sh cost                     estimate run cost so far (FLEET × uptime × flavor €/h)
#   ./box.sh down                     scale to zero (delete every VM + floating IP)
#   ./box.sh launch <detect args>     up → run, detached — kick off the fleet and walk away
#                                     (boxes stay UP; finish later with archive|publish|down)
#   ./box.sh all <detect args>        up → run → verify → archive → pull → down, hands-off
#   ./box.sh ssh [i] | ip | watch     interactive login to member i / floating IPs / re-attach
#
# `run`/`pull`/`watch` are ssh-only; `up`/`down`/`archive`/`ip` use the openstack
# API via the vendored 2fa openrc — non-interactive when a gitignored .env (repo
# root or here) sets CLOUDFERRO_PASSWORD + CLOUDFERRO_TOTP_SECRET (the base32
# authenticator seed); we feed the password and an oathtool code into its prompts.
# quote .env values (CLOUDFERRO_PASSWORD='p@ss$word') so the shell reads them
# verbatim. the box reads eodata with per-VM creds it pulls from the metadata
# service at boot (cloud-init), so detection itself needs no secrets.
set -euo pipefail
SELF_PWD="$PWD"             # caller's cwd — to resolve relative --aoi paths after the cd below
cd "$(dirname "$0")"

# GPU=1 → the full-tile nvJPEG2000 path: gpu cloud-init + an L40S vGPU flavor + the
# NVIDIA driver image, and `run`/`parity` build with --features gpu. Override
# FLAVOR/IMAGE/RATE for another gpu line (WAW3-2 quota here fits vm.l40s.1/.2; the
# passthrough gpu.* flavors need a quota bump). The cpu path is unchanged (GPU unset).
if [ "${GPU:-}" = 1 ]; then
  : "${CLOUD_INIT:=cloud-init-gpu.yaml}"; : "${FLAVOR:=vm.l40s.1}"
  : "${IMAGE:=Ubuntu 22.04 NVIDIA}"; : "${RATE:=0.45}"; : "${FLEET:=1}"
fi
: "${CLOUD_INIT:=cloud-init.yaml}"
: "${VM:=s2-flares}"            # base name; fleet members are $VM-0 … $VM-(FLEET-1)
: "${FLEET:=4}"                 # fleet size for bulk (--aoi) runs; a bbox run forces 1
: "${FLAVOR:=eo1.large}"
: "${IMAGE:=Ubuntu 22.04 LTS}"
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

# provision shared infra once (keypair/secgroup/network), then boot the FLEET members
# IN PARALLEL — each first boot is ~5min, so fan-out is the whole point. each member
# gets a floating IP cached in .box-ip-<i>; an existing member is reused (idempotent).
up(){
  auth
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
  local netid eoid; netid=$(openstack network show "$NET" -f value -c id); eoid=$(openstack network show "$EODATANET" -f value -c id)
  say "Booting fleet of $FLEET ($FLAVOR) in parallel — a few minutes…"
  local i; for i in $(seq 0 $((FLEET-1))); do boot_member "$i" "$netid" "$eoid" & done; wait
  printf '\n\033[1;32m✓ provisioned %s members\033[0m  →  ./box.sh ssh [i]   (first boot ~5min: rust / gdal / duckdb / build)\n' "$FLEET"
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

# ephemeral boxes, recycled floating IPs → skip host-key pinning entirely (see
# SSHOPTS). `exec` hands the process to ssh so the interactive session doesn't slurp
# the rest of this script's stdin. optional member index (default the head, 0).
go_ssh(){ exec ssh $SSHOPTS -i "$KEYFILE" "eouser@$(mip "${1:-0}")"; }

# shard the --aoi across the fleet (round-robin → balanced; a tile two terminals share
# landing on two boxes is fine — each writes its own <id>/ dir and the rollup dedups),
# scp each shard to its member, rebuild (git pull → latest detector) and launch the
# detached resumable detect on every member IN PARALLEL. a bbox/no-aoi run can't be
# split → one member. .fleet pins how many ran so later ops fan out to match.
run(){
  local a=("$@") aoi="" rest=() i f
  for ((i=0; i<${#a[@]}; i++)); do
    if [ "${a[i]}" = "--aoi" ]; then
      f="${a[i+1]:-}"; [ -f "$f" ] || f="$SELF_PWD/$f"   # box.sh cd'd to its own dir; resolve against the caller's
      if [ -f "$f" ]; then aoi="$f"; i=$((i+1)); continue; fi   # i++ returns 0 at i=0 → set -e abort on old bash
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
# one member: wait for cloud-init (a FRESH box installs rust/gdal + does a first build,
# ~5-8min, AFTER `server create --wait` returns ACTIVE — so launch/all can't build until
# the toolchain + a built tree exist), upload its shard, rebuild, launch the detached
# detect. run under `&`. the readiness poll also rides out sshd not-yet-up (mssh fails →
# retry); capped at ~20min so a wedged boot surfaces rather than hangs the fleet.
start_member(){
  local i=$1 aoi=$2 feat=$3 cudaenv=$4; shift 4; local rest=("$@") ip aoiarg="" w=0; ip=$(mip "$i")
  say "  [$i] waiting for cloud-init (rust/gdal/first build)…"
  until mssh "$i" 'test -f "$HOME/.cargo/env" && test -x '"$REPO_DIR"'/target/release/s2-flares' 2>/dev/null; do
    w=$((w+1)); [ "$w" -gt 120 ] && { echo "  [$i] not ready after ~20min — check ./box.sh ssh $i" >&2; return 1; }; sleep 10
  done
  if [ -n "$aoi" ]; then
    scp -q $SSHOPTS -i "$KEYFILE" "/tmp/_shard-$i.geojson" "eouser@$ip:$REPO_DIR/_aoi.geojson"
    aoiarg="--aoi _aoi.geojson"; fi
  say "  [$i] build (incremental)$feat"
  mssh "$i" "cd $REPO_DIR && git pull -q && . \$HOME/.cargo/env && ${cudaenv}cargo build --release -q -p s2-flares-cli$feat"
  # stop any detect already running on this member first → re-run is idempotent (resumable),
  # never two detects racing on the same per-scene CSVs. then launch the fresh one detached.
  mssh "$i" "pkill -x s2-flares 2>/dev/null; sleep 1; cd $REPO_DIR && nohup bash -lc './target/release/s2-flares detect --source cdse $aoiarg ${rest[*]} --out $OUT' >/tmp/cfrun.log 2>&1 & sleep 1; echo '  [$i] detect started'"
}
# round-robin split → /tmp/_shard-<i>.geojson (balanced slices; keeps the cli generic).
shard_aoi(){ python3 - "$1" "$2" <<'PY'
import json,sys
fs=json.load(open(sys.argv[1]))['features']; n=int(sys.argv[2])
for i in range(n): json.dump({'type':'FeatureCollection','features':fs[i::n]}, open(f'/tmp/_shard-{i}.geojson','w'))
PY
}

# stream every member's detect log (prefixed [i]) until ALL have printed `done:` — or a
# member's detect vanished after producing output (a crash). unbounded like the single-
# box watch: a wide run outlasts any fixed cap, and a cap would let `all` fall through
# mid-run. one ssh per member per cycle returns the new slice then a 0x1e-prefixed
# `<lines> <state>` tag (RS never occurs in a log line, so the split is unambiguous).
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

# the gpu↔cpu parity gate (gpu box only): assert nvJPEG2000 detections == GDAL/OpenJPEG
# detections byte-for-byte over real scenes. PARITY_BBOX a small test region; optional
# PARITY_TILE/START/END narrow it. lossless JP2 → identical pixels → identical core output.
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

# roll the per-scene CSVs up into BOTH published artifacts in one pass:
#   detections/  the raw archive — hive parquet s3://$BUCKET/detections/mgrs=…/
#                data.parquet, ONE deterministic-key parquet PER TILE (not per scene).
#                each tile file is SELECT DISTINCT over every AOI's CSVs for that tile
#                (cross-AOI/cross-shard union + dedup), ORDER BY date so row-group date
#                stats prune within a file.
#   clusters/    the derived VIEW — s2-flares cluster over the fresh detections/ →
#                clusters/data.parquet (one row/cluster + nested detections list). the
#                web map still re-clusters live in wasm for arbitrary windows.
#   coverage/    the scan FOOTPRINT — coverage/data.parquet, one row per (tile, quarter)
#                processed + its detection bbox, gating the web map's "Detect" button.
# FLEET note: each worker holds only its shard's CSVs, so we first GATHER every member's
# CSVs onto the head (member 0) — a single rollup there sees the whole archive at once,
# giving a clean per-tile DISTINCT (no cross-box last-write-wins when adjacent terminals
# share a tile) AND a complete coverage footprint. then duckdb (rollup) + the cli
# (cluster, with the site-anchored clear-sky persistence scan) both run on the head,
# in-region, carrying TWO credential sets: gdal /vsis3 reads SCL from eodata (AWS_* from
# the per-VM profile) for the coverage scan, while duckdb reads detections / writes
# clusters on the project bucket (S2_S3_* — kept separate so they don't collide).
archive(){
  auth
  openstack container show "$BUCKET" >/dev/null 2>&1 || { say "Bucket $BUCKET"; openstack container create "$BUCKET" >/dev/null; }
  local ak sk; read -r ak sk < <(s3creds)
  local n i; n=$(fleetn)
  # gather workers' CSVs → head. tar piped through the local host (workers can't ssh
  # each other; CSVs are tiny so this is cheap). different <id>/ dirs → no collision.
  for ((i=1; i<n; i++)); do
    say "Gather $(mvm "$i") CSVs → head"
    mssh "$i" "cd $REPO_DIR/$OUT 2>/dev/null && tar cf - . || true" | mssh 0 "mkdir -p $REPO_DIR/$OUT && tar xf - -C $REPO_DIR/$OUT"
  done
  say "Archive $OUT → s3://$BUCKET/{detections (per-tile parquet),coverage (scan footprint)}"
  local b64; b64=$(printf '%s' "$ARCHIVER" | base64 | tr -d '\n')
  mssh 0 "echo $b64 | base64 -d | AK='$ak' SK='$sk' REGION='$OS_REGION_NAME' BUCKET='$BUCKET' OUT='$REPO_DIR/$OUT' bash"
  say "Cluster view (+ site-anchored clear-sky persistence) → s3://$BUCKET/clusters/data.parquet"
  mssh 0 "cd $REPO_DIR && . /etc/profile.d/eodata.sh && \
        S2_S3_ENDPOINT='s3.$OS_REGION_NAME.cloudferro.com' S2_S3_REGION='$OS_REGION_NAME' \
        S2_S3_ACCESS_KEY='$ak' S2_S3_SECRET_KEY='$sk' \
        ./target/release/s2-flares cluster --source cdse --concurrency ${COVERAGE_CONCURRENCY:-16} \
          --archive 's3://$BUCKET/detections/**/*.parquet' --coverage-scan '$OUT/coverage' \
          --out 's3://$BUCKET/clusters/data.parquet' --start '${START:-2015-01-01}' --end '${END:-2100-01-01}'"
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
  # coverage/ the scan FOOTPRINT — one row per (tile, quarter) actually processed,
  # with that tile-quarter's detection bbox. the web map folds these rects into its
  # "already detected" test so the Detect button only lights over archive gaps. read
  # over EVERY scene CSV (not just the non-empty tile list): flareless scenes are
  # header-only so they add no rows — coverage thus UNDER-claims (the safe direction;
  # Detect stays enabled on a scanned-but-flareless tile, never wrongly disabled).
  echo "COPY (SELECT mgrs, year(CAST(date AS DATE)) AS y, quarter(CAST(date AS DATE)) AS q,
        min(lon) AS w, min(lat) AS s, max(lon) AS e, max(lat) AS n, count(*) AS n_det
        FROM read_csv('$OUT/*/*.csv', union_by_name=true) GROUP BY mgrs, y, q)
        TO 's3://$BUCKET/coverage/data.parquet' (FORMAT parquet);"
} | "$DDB"
"$DDB" -c "$S3 SELECT count(*) AS archived_rows, count(DISTINCT date) AS dates FROM read_parquet('s3://$BUCKET/detections/**/data.parquet', hive_partitioning=true);"
EOS
)

# make the archive a web-map backend: anonymous public-read on detections/* +
# clusters/* + coverage/* + CORS, so a browser (e.g. DuckDB-wasm) can range-read the
# parquet directly — scalar cluster pins from clusters/, raw-detection reclustering
# from detections/, the scan footprint (Detect-button gating) from coverage/. one-time
# per bucket; needs aws-cli (RadosGW S3 policy/CORS aren't openstack/swift operations).
publish(){
  auth
  command -v aws >/dev/null || { echo "publish needs aws-cli (brew install awscli)" >&2; exit 1; }
  local ak sk; read -r ak sk < <(s3creds)
  local aws_s3=(env AWS_ACCESS_KEY_ID="$ak" AWS_SECRET_ACCESS_KEY="$sk" AWS_DEFAULT_REGION="$OS_REGION_NAME"
    aws --endpoint-url "https://s3.$OS_REGION_NAME.cloudferro.com" --no-cli-pager s3api)
  say "Publishing s3://$BUCKET/{detections,clusters,coverage} for web-map access (public-read + CORS)"
  "${aws_s3[@]}" put-bucket-cors --bucket "$BUCKET" --cors-configuration '{"CORSRules":[{"AllowedOrigins":["*"],"AllowedMethods":["GET","HEAD"],"AllowedHeaders":["*"],"ExposeHeaders":["Content-Range","Content-Length","ETag","Accept-Ranges"],"MaxAgeSeconds":3600}]}'
  "${aws_s3[@]}" put-bucket-policy --bucket "$BUCKET" --policy '{"Version":"2012-10-17","Statement":[{"Sid":"PublicReadArchive","Effect":"Allow","Principal":{"AWS":["*"]},"Action":["s3:GetObject"],"Resource":["arn:aws:s3:::'"$BUCKET"'/detections/*","arn:aws:s3:::'"$BUCKET"'/clusters/*","arn:aws:s3:::'"$BUCKET"'/coverage/*"]},{"Sid":"PublicListArchive","Effect":"Allow","Principal":{"AWS":["*"]},"Action":["s3:ListBucket"],"Resource":["arn:aws:s3:::'"$BUCKET"'"]}]}'
  echo "  public read + CORS applied. objects at https://s3.$OS_REGION_NAME.cloudferro.com/$BUCKET/{detections,clusters,coverage}/…"
}

# empty the archive bucket (every object; the bucket stays) — e.g. to start fresh
# after a layout change orphans the old prefixes. IRREVERSIBLE: aws s3 rm doesn't
# prompt, so we do, requiring the bucket name typed back (FORCE=1 skips, for scripts).
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

# run cost, instantly: CloudFerro bills the flavor by the hour, so FLEET × uptime ×
# RATE is a deterministic local estimate — the billing portal aggregates daily, far
# too coarse/slow to reflect a run in flight. assumes auth; `cost` wraps it with auth.
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

# PROVE a run is complete and clean, per member, BEFORE the gather/archive — two
# assertions over each box's OUT (born of the run that silently hid 25 of 81 LNG
# terminals from the published archive):
#   1. coverage — every AOI feature in this member's shard was reached. detect writes
#      OUT/<id>/<mgrs>_<date>.csv (header-only even when flareless), so a feature with
#      NO subdir == never scanned. id precedence mirrors load_aois() in main.rs.
#   2. no errors — every attempted scene succeeded. a read/detect FAIL leaves a sibling
#      <mgrs>_<date>.err (cleared on a later successful retry); ANY remaining .err means
#      a scene is unproven. the path to green is just: re-run (resumable) until clean.
# verifies each member against its OWN shard (_aoi.geojson); summed, that is the whole
# AOI. exits nonzero if any member has a gap/error; `all` gates the teardown on it.
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

# combined kick-off — provision the fleet AND start the detached, sharded detection in
# one command, then leave the boxes UP. our most common entrypoint: fire a run over a
# few terminals and come back later to archive|publish|down. unlike `all` it does NOT
# archive/scale-to-zero — `run` already detaches per box (nohup), so the local watch
# streaming is severable (Ctrl-C / close the session) without stopping the runs.
launch(){ up; run "$@"; }

# hands-off pipeline: provision the fleet, detect (sharded), prove it, archive to object
# storage (gather + coverage), pull locally, scale to zero. the archive (S3) and local
# CSVs persist; only the boxes are ephemeral. `down` fires only when `verify` proves
# every AOI feature was scanned — a gap keeps the fleet up for a resumable re-run.
all(){ up; run "$@"
  if verify; then archive; pull; down
  else say "verify found unscanned AOI features — fleet kept up. re-run (resumable), then ./box.sh archive && ./box.sh down"; fi; }

case "${1:-}" in
  up) up;; ip) ip;; ssh) shift; go_ssh "${1:-0}";; cost) cost;; down) down;;
  run) shift; run "$@";; watch) watch;; pull) pull;; archive) archive;; verify) verify;; publish) publish;; wipe) wipe;; parity) parity;;
  launch) shift; launch "$@";; all) shift; all "$@";;
  *) echo "usage: $0 {up | run <args> | launch <args> | watch | pull | archive | verify | publish | wipe | parity | cost | down | all <args> | ssh [i] | ip}  (FLEET=N, default 4; GPU=1 → gpu box)" >&2; exit 1;;
esac
