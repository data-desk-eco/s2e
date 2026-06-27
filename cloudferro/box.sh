#!/usr/bin/env bash
# CloudFerro WAW3-2 end-to-end orchestration, co-located with the Copernicus
# `eodata` archive (free in-region JP2 reads). One script, one auth path:
#
#   ./box.sh up                       provision the box (keypair/secgroup/net/VM/IP)
#   ./box.sh run <cf-run args>        detached, resumable detection + live progress
#   ./box.sh pull                     rsync the per-scene CSVs down to $LOCAL_DATA
#   ./box.sh archive                  push a growing per-tile parquet collection to
#                                     CloudFerro object storage (s3://$BUCKET/flares)
#   ./box.sh down                     scale to zero (delete VM + floating IP)
#   ./box.sh all <cf-run args>        up → run → archive → pull → down, hands-off
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
: "${OUT:=out}"                 # box-side output dir (cf-run writes <OUT>/<id>/<scene>.csv)
: "${LOCAL_DATA:=../data/cf}"   # where `pull` lands the CSVs
: "${BUCKET:=s2-flares-archive}"  # CloudFerro object-storage container for `archive`
: "${PRESET:=loose}"            # tags the archive partition (detector preset)

SSHOPTS="-o LogLevel=ERROR -o StrictHostKeyChecking=accept-new -o UserKnownHostsFile=/dev/null"
say(){ printf '\033[1;36m→ %s\033[0m\n' "$*"; }
boxip(){ cat .box-ip 2>/dev/null || { echo "no box — run ./box.sh up" >&2; exit 1; }; }
sshx(){ ssh $SSHOPTS -i "$KEYFILE" "eouser@$(boxip)" "$@"; }

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
      --user-data cloud-init.yaml --wait >/dev/null
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
  printf '\n\033[1;32m✓ provisioned\033[0m  →  ./box.sh ssh   (first boot ~3min: node 22 / gdal / duckdb / clone)\n   ssh -i %s eouser@%s\n' "$KEYFILE" "$fip"
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

# detached + resumable: cf-run skips scenes whose CSV already exists, so a dropped
# connection or a re-run just continues. an --aoi geojson is uploaded transparently.
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
  say "Detection on $ip (detached, resumable) → $REPO_DIR/$OUT"
  sshx "cd $REPO_DIR && git pull -q && nohup bash -lc 'node cf-run.js ${args[*]} --out $OUT --preset $PRESET' >/tmp/cfrun.log 2>&1 & sleep 1; echo '  started — streaming progress (Ctrl-C is safe, the run continues)'"
  watch
}

# stream new cf-run log lines until the run prints its `done:` summary.
watch(){
  sshx 'log=/tmp/cfrun.log; n=0
    for _ in $(seq 1 2400); do
      t=$(wc -l <"$log" 2>/dev/null || echo 0)
      [ "$t" -gt "$n" ] && { sed -n "$((n+1)),${t}p" "$log"; n=$t; }
      grep -q "^done:" "$log" 2>/dev/null && break
      sleep 3
    done'
}

pull(){
  local ip; ip=$(boxip); mkdir -p "$LOCAL_DATA"
  say "Pull $REPO_DIR/$OUT → $LOCAL_DATA"
  rsync -az -e "ssh $SSHOPTS -i $KEYFILE" "eouser@$ip:$REPO_DIR/$OUT/" "$LOCAL_DATA/"
  echo "  $(find "$LOCAL_DATA" -name '*.csv' | wc -l | tr -d ' ') scene CSVs in $LOCAL_DATA"
}

# grow a hive-partitioned parquet collection on CloudFerro object storage —
# s3://$BUCKET/flares/preset=…/mgrs=…/date=…/ — the same per-tile archive the web
# api scene-store keeps, queryable in one read_parquet(…, hive_partitioning=true).
# duckdb runs on the box (in-region write); project S3 creds come from openstack ec2.
archive(){
  auth
  openstack container show "$BUCKET" >/dev/null 2>&1 || { say "Bucket $BUCKET"; openstack container create "$BUCKET" >/dev/null; }
  local creds ak sk
  creds=$(openstack ec2 credentials list -f json | jq -r '.[0]|"\(.Access) \(.Secret)"' 2>/dev/null || true)
  { [ -z "${creds:-}" ] || [ "$creds" = "null null" ]; } && { openstack ec2 credentials create >/dev/null; creds=$(openstack ec2 credentials list -f json | jq -r '.[0]|"\(.Access) \(.Secret)"'); }
  read -r ak sk <<<"$creds"
  say "Archive $OUT → s3://$BUCKET/flares (per-tile parquet, preset=$PRESET)"
  sshx "~/.duckdb/cli/latest/duckdb -c \"
    INSTALL httpfs; LOAD httpfs;
    SET s3_endpoint='s3.$OS_REGION_NAME.cloudferro.com'; SET s3_region='$OS_REGION_NAME'; SET s3_url_style='path'; SET s3_use_ssl=true;
    SET s3_access_key_id='$ak'; SET s3_secret_access_key='$sk';
    COPY (SELECT *, '$PRESET' AS preset FROM read_csv('$REPO_DIR/$OUT/**/*.csv', union_by_name=true))
      TO 's3://$BUCKET/flares' (FORMAT parquet, PARTITION_BY (preset, mgrs, date), OVERWRITE_OR_IGNORE, FILENAME_PATTERN 'part_{uuid}');
    SELECT count(*) AS archived_rows, count(DISTINCT date) AS dates FROM read_parquet('s3://$BUCKET/flares/**/*.parquet', hive_partitioning=true);\""
}

down(){
  auth
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
  up) up;; ip) ip;; ssh) go_ssh;; down) down;;
  run) shift; run "$@";; watch) watch;; pull) pull;; archive) archive;;
  all) shift; all "$@";;
  *) echo "usage: $0 {up | run <cf-run args> | watch | pull | archive | down | all <cf-run args> | ssh | ip}" >&2; exit 1;;
esac
