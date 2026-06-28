#!/usr/bin/env bash
# CloudFerro WAW3-2 end-to-end orchestration, co-located with the Copernicus
# `eodata` archive (free in-region JP2 reads). One script, one auth path:
#
#   ./box.sh up                       provision the box (keypair/secgroup/net/VM/IP)
#   ./box.sh run <detect args>        detached, resumable detection + live progress
#   ./box.sh pull                     rsync the per-scene CSVs down to $LOCAL_DATA
#   ./box.sh archive                  roll the CSVs up to s3://$BUCKET/{detections,
#                                     clusters} (per-tile raw parquet + the view)
#   ./box.sh publish                  make that archive a web-map backend: anonymous
#                                     public-read + CORS, so DuckDB-wasm can read it
#   ./box.sh wipe                     empty the archive bucket (confirms; FORCE=1 skips)
#   ./box.sh cost                     estimate run cost so far (uptime × flavor €/h)
#   ./box.sh down                     scale to zero (delete VM + floating IP)
#   ./box.sh all <detect args>        up → run → archive → pull → down, hands-off
#   ./box.sh ssh | ip | watch         interactive login / floating IP / re-attach
#
# `run`/`pull`/`watch` are ssh-only; `up`/`down`/`archive`/`ip` use the openstack
# API via the vendored 2fa openrc — non-interactive when a gitignored .env (repo
# root or here) sets CLOUDFERRO_PASSWORD + CLOUDFERRO_TOTP_SECRET (the base32
# authenticator seed); we feed the password and an oathtool code into its prompts.
# quote .env values (CLOUDFERRO_PASSWORD='p@ss$word') so the shell reads them
# verbatim. the box reads eodata with per-VM creds it pulls from the metadata
# service at boot (cloud-init), so detection itself needs no secrets.
set -euo pipefail
cd "$(dirname "$0")"

# GPU=1 → the full-tile nvJPEG2000 path: gpu cloud-init + an L40S vGPU flavor + the
# NVIDIA driver image, and `run`/`parity` build with --features gpu. Override
# FLAVOR/IMAGE/RATE for another gpu line (WAW3-2 quota here fits vm.l40s.1/.2; the
# passthrough gpu.* flavors need a quota bump). The cpu path is unchanged (GPU unset).
if [ "${GPU:-}" = 1 ]; then
  : "${CLOUD_INIT:=cloud-init-gpu.yaml}"; : "${FLAVOR:=vm.l40s.1}"
  : "${IMAGE:=Ubuntu 22.04 NVIDIA}"; : "${RATE:=0.45}"
fi
: "${CLOUD_INIT:=cloud-init.yaml}"
: "${VM:=s2-flares}"
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
boxip(){ cat .box-ip 2>/dev/null || { echo "no box — run ./box.sh up" >&2; exit 1; }; }
sshx(){ ssh $SSHOPTS -i "$KEYFILE" "eouser@$(boxip)" "$@"; }
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
  if openstack server show "$VM" >/dev/null 2>&1; then say "VM $VM already exists"; else
    say "Booting VM $VM ($FLAVOR) — a few minutes…"
    openstack server create "$VM" \
      --flavor "$FLAVOR" --image "$IMAGE" --key-name "$KEYPAIR" --security-group "$SECGROUP" \
      --nic net-id="$(openstack network show "$NET" -f value -c id)" \
      --nic net-id="$(openstack network show "$EODATANET" -f value -c id)" \
      --user-data "$CLOUD_INIT" --wait >/dev/null
  fi
  say "Floating IP"
  local port fip
  port=$(openstack port list --server "$VM" --network "$NET" -f value -c id | head -1)
  fip=$(openstack floating ip list --port "$port" -f value -c "Floating IP Address" | head -1)
  if [ -z "$fip" ]; then
    fip=$(openstack floating ip create "$EXTNET" -f value -c floating_ip_address)
    openstack floating ip set --port "$port" "$fip" >/dev/null
  fi
  echo "$fip" > .box-ip
  printf '\n\033[1;32m✓ provisioned\033[0m  →  ./box.sh ssh   (first boot ~5min: rust / gdal / duckdb / build)\n   ssh -i %s eouser@%s\n' "$KEYFILE" "$fip"
}

ip(){
  auth
  local port
  port=$(openstack port list --server "$VM" --network "$NET" -f value -c id | head -1)
  openstack floating ip list --port "$port" -f value -c "Floating IP Address" | head -1 | tee .box-ip
}

# ephemeral box, recycled floating IPs → skip host-key pinning entirely (see
# SSHOPTS). `exec` hands the process to ssh so the interactive session doesn't
# slurp the rest of this script's stdin (which caused a stray syntax error on logout).
go_ssh(){ exec ssh $SSHOPTS -i "$KEYFILE" "eouser@$(boxip)"; }

# detached + resumable: `s2-flares detect` skips scenes whose CSV already exists, so
# a dropped connection or a re-run just continues. an --aoi geojson is uploaded
# transparently. the native cli is rebuilt first (incremental — fast if unchanged)
# so a git pull takes effect; --source cdse selects the eodata jp2 reader.
run(){
  local ip; ip=$(boxip)
  local a=("$@") args=() i
  for ((i=0; i<${#a[@]}; i++)); do
    if [ "${a[i]}" = "--aoi" ] && [ -f "${a[i+1]:-}" ]; then
      say "Uploading AOI ${a[i+1]}"
      scp -q $SSHOPTS -i "$KEYFILE" "${a[i+1]}" "eouser@$ip:$REPO_DIR/_aoi.geojson"
      args+=(--aoi _aoi.geojson); ((i++))
    else args+=("${a[i]}"); fi
  done
  # gpu box: build with the feature, source the cuda env (nvcc/CUDA_PATH live in
  # /etc/profile.d/cuda.sh — not on the non-login ssh PATH), and select the gpu reader
  # at runtime (the --features build only COMPILES it; --gpu picks it) unless already set.
  local feat="" cudaenv=""
  if [ "${GPU:-}" = 1 ]; then
    feat=" --features gpu"; cudaenv=". /etc/profile.d/cuda.sh && "
    [[ " ${args[*]} " == *" --gpu "* ]] || args+=(--gpu)
  fi
  say "Build on $ip (incremental)$feat"
  sshx "cd $REPO_DIR && git pull -q && . \$HOME/.cargo/env && ${cudaenv}cargo build --release -q -p s2-flares-cli$feat"
  say "Detection on $ip (detached, resumable) → $REPO_DIR/$OUT"
  sshx "cd $REPO_DIR && nohup bash -lc './target/release/s2-flares detect --source cdse ${args[*]} --out $OUT' >/tmp/cfrun.log 2>&1 & sleep 1; echo '  started — streaming progress (Ctrl-C is safe, the run continues)'"
  watch
}

# stream new detect log lines until the run prints its `done:` summary. unbounded by
# design — a wide-area run can exceed any fixed cap, and a fixed cap would make `all`
# fall through to archive/pull/down MID-RUN. we exit only on `done:`, or on the detect
# binary vanishing (crash) once it has produced output. pgrep -x matches the detect
# comm `s2-flares`, not this watcher; the n>0 guard avoids the launch race.
watch(){
  sshx 'log=/tmp/cfrun.log; n=0
    while :; do
      [ -f "$log" ] || { sleep 3; continue; }   # run not yet launched — wait, do not leak the input-redirect error
      t=$(wc -l <"$log" 2>/dev/null || echo 0)
      [ "$t" -gt "$n" ] && { sed -n "$((n+1)),${t}p" "$log"; n=$t; }
      grep -q "^done:" "$log" 2>/dev/null && break
      pgrep -x s2-flares >/dev/null 2>&1 || { [ "$n" -gt 0 ] && { echo "watch: detect exited without done: — see $log"; break; }; }
      sleep 3
    done'
}

# the gpu↔cpu parity gate (gpu box only): assert nvJPEG2000 detections == GDAL/OpenJPEG
# detections byte-for-byte over real scenes. PARITY_BBOX a small test region; optional
# PARITY_TILE/START/END narrow it. lossless JP2 → identical pixels → identical core output.
parity(){
  : "${PARITY_BBOX:?set PARITY_BBOX=W,S,E,N (a small test region)}"
  local ip; ip=$(boxip)
  say "Parity gpu-vs-cpu on $ip"
  sshx "cd $REPO_DIR && git pull -q && . \$HOME/.cargo/env && . /etc/profile.d/cuda.sh && . /etc/profile.d/eodata.sh && \
    S2_PARITY_BBOX='$PARITY_BBOX' ${PARITY_TILE:+S2_PARITY_TILE='$PARITY_TILE'} ${START:+S2_PARITY_START='$START'} ${END:+S2_PARITY_END='$END'} \
    cargo test --release -p s2-flares-cli --features gpu parity -- --ignored --nocapture"
}

pull(){
  local ip; ip=$(boxip); mkdir -p "$LOCAL_DATA"
  say "Pull $REPO_DIR/$OUT → $LOCAL_DATA"
  rsync -az -e "ssh $SSHOPTS -i $KEYFILE" "eouser@$ip:$REPO_DIR/$OUT/" "$LOCAL_DATA/"
  echo "  $(find "$LOCAL_DATA" -name '*.csv' | wc -l | tr -d ' ') scene CSVs in $LOCAL_DATA"
}

# roll the per-scene CSVs up into BOTH published artifacts in one pass:
#   detections/  the raw archive — hive parquet s3://$BUCKET/detections/mgrs=…/
#                data.parquet, ONE deterministic-key parquet PER TILE (not per scene).
#                resumability is the per-scene CSV layer (presence==done), so the
#                parquet rollup is free to coarsen — per-tile gives ~10²-not-10⁴
#                objects of a useful size, far fewer footer reads for bulk scans and
#                the web map's range requests. each tile file is SELECT DISTINCT over
#                every AOI's CSVs for that tile (cross-AOI detections union + dedup —
#                the AOI-agnostic data model — vs the old per-scene last-write-wins),
#                ORDER BY date so row-group date stats prune within a file.
#   clusters/    the derived VIEW — s2-flares cluster over the fresh detections/ →
#                clusters/data.parquet (one row/cluster + nested detections list).
#                co-produced here, not in a separate run; the web map still re-clusters
#                live in wasm for arbitrary windows. clustered full-window (START/END).
# duckdb (rollup) + the cli (cluster) both run on the box, in-region; mgrs lives in the
# detections path (EXCLUDEd), date stays a column; project S3 creds from openstack ec2.
archive(){
  auth
  openstack container show "$BUCKET" >/dev/null 2>&1 || { say "Bucket $BUCKET"; openstack container create "$BUCKET" >/dev/null; }
  local ak sk; read -r ak sk < <(s3creds)
  say "Archive $OUT → s3://$BUCKET/detections (per-tile parquet)"
  local b64; b64=$(printf '%s' "$ARCHIVER" | base64 | tr -d '\n')
  sshx "echo $b64 | base64 -d | AK='$ak' SK='$sk' REGION='$OS_REGION_NAME' BUCKET='$BUCKET' OUT='$REPO_DIR/$OUT' bash"
  say "Cluster view → s3://$BUCKET/clusters/data.parquet"
  sshx "S2_S3_ENDPOINT='s3.$OS_REGION_NAME.cloudferro.com' S2_S3_REGION='$OS_REGION_NAME' \
        AWS_ACCESS_KEY_ID='$ak' AWS_SECRET_ACCESS_KEY='$sk' \
        s2-flares cluster --archive 's3://$BUCKET/detections/**/*.parquet' \
          --out 's3://$BUCKET/clusters/data.parquet' --start '${START:-2015-01-01}' --end '${END:-2100-01-01}'"
}

# runs on the box: one COPY per tile (union of all that tile's non-empty scene CSVs)
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
} | "$DDB"
"$DDB" -c "$S3 SELECT count(*) AS archived_rows, count(DISTINCT date) AS dates FROM read_parquet('s3://$BUCKET/detections/**/data.parquet', hive_partitioning=true);"
EOS
)

# make the archive a web-map backend: anonymous public-read on detections/* +
# clusters/* + CORS, so a browser (e.g. DuckDB-wasm) can range-read the parquet
# directly — scalar cluster pins from clusters/, raw-detection reclustering from
# detections/. one-time per bucket; needs aws-cli (RadosGW S3 policy/CORS aren't
# openstack/swift operations).
publish(){
  auth
  command -v aws >/dev/null || { echo "publish needs aws-cli (brew install awscli)" >&2; exit 1; }
  local ak sk; read -r ak sk < <(s3creds)
  local aws_s3=(env AWS_ACCESS_KEY_ID="$ak" AWS_SECRET_ACCESS_KEY="$sk" AWS_DEFAULT_REGION="$OS_REGION_NAME"
    aws --endpoint-url "https://s3.$OS_REGION_NAME.cloudferro.com" --no-cli-pager s3api)
  say "Publishing s3://$BUCKET/{detections,clusters} for web-map access (public-read + CORS)"
  "${aws_s3[@]}" put-bucket-cors --bucket "$BUCKET" --cors-configuration '{"CORSRules":[{"AllowedOrigins":["*"],"AllowedMethods":["GET","HEAD"],"AllowedHeaders":["*"],"ExposeHeaders":["Content-Range","Content-Length","ETag","Accept-Ranges"],"MaxAgeSeconds":3600}]}'
  "${aws_s3[@]}" put-bucket-policy --bucket "$BUCKET" --policy '{"Version":"2012-10-17","Statement":[{"Sid":"PublicReadArchive","Effect":"Allow","Principal":{"AWS":["*"]},"Action":["s3:GetObject"],"Resource":["arn:aws:s3:::'"$BUCKET"'/detections/*","arn:aws:s3:::'"$BUCKET"'/clusters/*"]}]}'
  echo "  public read + CORS applied. objects at https://s3.$OS_REGION_NAME.cloudferro.com/$BUCKET/{detections,clusters}/…"
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

# run cost, instantly: CloudFerro bills the flavor by the hour, so uptime × RATE is
# a deterministic local estimate — the billing portal aggregates daily, far too
# coarse/slow to reflect a run in flight. assumes auth; `cost` wraps it with auth.
costline(){
  local t; t=$(openstack server show "$VM" -f value -c created 2>/dev/null) || return 1
  t=${t%Z}; t=${t%.*}; t=$(date -ju -f "%Y-%m-%dT%H:%M:%S" "$t" +%s 2>/dev/null || date -u -d "$t" +%s)
  local h; h=$(echo "scale=2;($(date -u +%s)-$t)/3600" | bc)
  printf '\033[1;36m→ %s up %sh × €%s/h ≈ €%s\033[0m\n' "$FLAVOR" "$h" "$RATE" "$(echo "scale=2;$h*$RATE"|bc)"
}
cost(){ auth; costline || echo "no VM $VM" >&2; }

down(){
  auth
  costline || true
  local port fip=""
  port=$(openstack port list --server "$VM" --network "$NET" -f value -c id 2>/dev/null | head -1 || true)
  [ -n "$port" ] && fip=$(openstack floating ip list --port "$port" -f value -c "Floating IP Address" | head -1 || true)
  say "Deleting VM $VM"
  openstack server delete "$VM" --wait >/dev/null 2>&1 && echo "  deleted" || echo "  no VM $VM"
  [ -n "$fip" ] && { say "Releasing floating IP $fip"; openstack floating ip delete "$fip" >/dev/null 2>&1 || true; }
  rm -f .box-ip
  printf '\n\033[1;32m✓ scaled to zero\033[0m\n'
}

# hands-off pipeline: provision, detect, archive to object storage, pull locally,
# scale to zero. the archive (S3) and the local CSVs persist; only the box is
# ephemeral. compose the steps by hand to keep the box warm between runs.
all(){ up; run "$@"; archive; pull; down; }

case "${1:-}" in
  up) up;; ip) ip;; ssh) go_ssh;; cost) cost;; down) down;;
  run) shift; run "$@";; watch) watch;; pull) pull;; archive) archive;; publish) publish;; wipe) wipe;; parity) parity;;
  all) shift; all "$@";;
  *) echo "usage: $0 {up | run <detect args> | watch | pull | archive | publish | wipe | parity | cost | down | all <detect args> | ssh | ip}  (GPU=1 → gpu box)" >&2; exit 1;;
esac
