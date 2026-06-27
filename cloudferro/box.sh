#!/usr/bin/env bash
# CloudFerro WAW3-2 box lifecycle, co-located with the Copernicus `eodata`
# archive (free in-region JP2 reads).   usage: box.sh {up|ip|ssh|down}
#
# auth is the vendored official 2fa openrc. it runs non-interactively when a
# gitignored .env (repo root or here) sets CLOUDFERRO_PASSWORD +
# CLOUDFERRO_TOTP_SECRET (the base32 authenticator seed) — we feed the password
# and an oathtool-generated code into the openrc's two prompts. otherwise it
# prompts. quote values in .env (e.g. CLOUDFERRO_PASSWORD='p@ss$word') so the
# shell reads them verbatim. the box reads eodata anonymously, so it needs no creds.
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

say(){ printf '\033[1;36m→ %s\033[0m\n' "$*"; }

auth(){
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
  printf '\n\033[1;32m✓ provisioned\033[0m  →  ./box.sh ssh   (first boot ~3min: node 22 / gdal / clone)\n   ssh -i %s eouser@%s\n' "$KEYFILE" "$fip"
}

ip(){
  auth
  local port
  port=$(openstack port list --server "$VM" --network "$NET" -f value -c id | head -1)
  openstack floating ip list --port "$port" -f value -c "Floating IP Address" | head -1 | tee .box-ip
}

# ephemeral box, recycled floating IPs → skip host-key pinning entirely:
# accept-new + a throwaway known_hosts means no "authenticity can't be
# established" prompt and no stale-key error when an IP is reused. LogLevel=ERROR
# hushes the cosmetic "no post-quantum key exchange" warning; real errors print.
go_ssh(){ ssh -o LogLevel=ERROR -o StrictHostKeyChecking=accept-new -o UserKnownHostsFile=/dev/null -i "$KEYFILE" "eouser@$(cat .box-ip)"; }

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

case "${1:-}" in
  up) up;; ip) ip;; ssh) go_ssh;; down) down;;
  *) echo "usage: $0 {up|ip|ssh|down}" >&2; exit 1;;
esac
