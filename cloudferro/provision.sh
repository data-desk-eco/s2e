#!/usr/bin/env bash
# provision the EU-sovereign bulk-detection box on CloudFerro WAW3-2, co-located
# with the Copernicus `eodata` archive (free in-region JP2 reads). one command:
# OIDC auth → reusable application credential → keypair + security group → VM on
# the eodata network with a floating IP → cloud-init installs node 22 / gdal /
# duckdb and clones s2-flares. idempotent: re-running reuses existing resources.
#
#   bash cloudferro/provision.sh           # full provision
#   DESTROY=1 bash cloudferro/provision.sh # tear the VM + floating ip down (scale to zero)
#
# auth note: this account is OIDC-federated (Keycloak) with 2FA — plain keystone
# password auth does NOT work (see memory project_cloudferro_auth). so we do the
# password+TOTP grant once, then mint an application credential that needs neither
# and drop it in ~/.config/openstack/clouds.yaml as cloud `cloudferro` for every
# later openstack call (and the box itself). re-auth only when the app cred expires.
set -euo pipefail

# --- config (all overridable) ------------------------------------------------
: "${OS_USERNAME:=louis@datadesk.eco}"
: "${OS_PROJECT_ID:=87f09f10653f4db9bd806c7f430f8962}"
: "${OS_PROJECT_DOMAIN_ID:=dc1b613a62c04f6eadb143360451cc18}"
: "${OS_REGION_NAME:=WAW3-2}"
: "${FLAVOR:=eo1.large}"                       # 4 vCPU / 8 GB; eo1.medium = 2/4
: "${IMAGE:=Ubuntu 22.04 LTS}"
: "${VM_NAME:=s2-flares}"
: "${SECGROUP:=s2-flares}"
: "${KEYPAIR:=s2-flares}"
: "${KEYFILE:=$HOME/.ssh/id_ed25519}"
: "${EODATA_NET:=eodata}"                      # provider net giving free eodata S3
: "${EXTERNAL_NET:=external}"                  # floating-ip pool
: "${PRIVATE_NET:=}"                           # auto-create/reuse a tenant net if empty
: "${REPO:=https://github.com/louisgv/s2-flares.git}"   # adjust to the real remote

ENVDIR="$HOME/.config/openstack"; CLOUDS="$ENVDIR/clouds.yaml"
KEYCLOAK=https://identity.cloudferro.com/auth/realms/CloudFerro-Cloud/protocol/openid-connect/token
CLIENT_ID=openstack CLIENT_SECRET=KVOWUhVvhjqqUpCdru4IghTHqmMkBt7S
osc() { openstack --os-cloud cloudferro "$@"; }   # all calls use the app-cred cloud

# --- 1. auth: password+TOTP → keystone token → application credential --------
# only runs if no usable `cloudferro` app-cred cloud exists yet.
ensure_appcred() {
  if [ -f "$CLOUDS" ] && grep -q '^\s*cloudferro:' "$CLOUDS" && \
     openstack --os-cloud cloudferro token issue >/dev/null 2>&1; then
    echo "✓ application credential present and valid"; return
  fi
  echo "no valid app credential — doing one interactive OIDC (password+TOTP) auth"
  local pw="${CLOUDFERRO_PASSWORD:-}"
  [ -z "$pw" ] && [ -f .env ] && pw="$(grep '^CLOUDFERRO_PASSWORD=' .env | cut -d= -f2-)"
  [ -z "$pw" ] && { read -rsp "OpenStack password: " pw; echo; }
  read -rp "2FA one-time code (blank if 2FA off): " totp
  local resp access
  resp="$(curl -s -X POST "$KEYCLOAK" -H 'Content-Type: application/x-www-form-urlencoded' \
    --data-urlencode "username=$OS_USERNAME" --data-urlencode "password=$pw" \
    -d "grant_type=password&client_id=$CLIENT_ID&client_secret=$CLIENT_SECRET${totp:+&totp=$totp}")"
  access="$(echo "$resp" | python3 -c 'import sys,json;print(json.load(sys.stdin).get("access_token",""))')"
  [ -z "$access" ] && { echo "auth failed: $resp" >&2; exit 1; }

  # exchange the keycloak token for a keystone token, then mint an app credential.
  export OS_AUTH_URL=https://keystone.cloudferro.com/v3 OS_IDENTITY_API_VERSION=3 \
    OS_AUTH_TYPE=v3oidcaccesstoken OS_ACCESS_TOKEN="$access" \
    OS_PROTOCOL=openid OS_IDENTITY_PROVIDER=ident_cloudferro-cloud_provider \
    OS_PROJECT_ID="$OS_PROJECT_ID" OS_PROJECT_DOMAIN_ID="$OS_PROJECT_DOMAIN_ID" \
    OS_REGION_NAME="$OS_REGION_NAME" OS_INTERFACE=public
  openstack token issue -f value -c id >/dev/null 2>&1 || \
    { echo "keystone OIDC exchange failed — token unusable. raw keycloak resp: $resp" >&2; exit 1; }
  openstack application credential delete s2-flares-box >/dev/null 2>&1 || true
  local cred; cred="$(openstack application credential create s2-flares-box \
    --unrestricted -f json 2>&1)"
  local id secret
  id="$(echo "$cred" | python3 -c 'import sys,json;print(json.load(sys.stdin)["id"])' 2>/dev/null)"
  secret="$(echo "$cred" | python3 -c 'import sys,json;print(json.load(sys.stdin)["secret"])' 2>/dev/null)"
  [ -z "$id" ] && { echo "app credential create failed:" >&2; echo "$cred" >&2; exit 1; }
  mkdir -p "$ENVDIR"
  python3 - "$CLOUDS" "$id" "$secret" "$OS_REGION_NAME" <<'PY'
import sys, os, yaml
path, cid, secret, region = sys.argv[1:5]
data = {}
if os.path.exists(path):
    with open(path) as f: data = yaml.safe_load(f) or {}
data.setdefault('clouds', {})['cloudferro'] = {
    'auth_type': 'v3applicationcredential',
    'auth': {'auth_url': 'https://keystone.cloudferro.com/v3',
             'application_credential_id': cid,
             'application_credential_secret': secret},
    'region_name': region, 'interface': 'public', 'identity_api_version': 3}
with open(path, 'w') as f: yaml.safe_dump(data, f)
print('wrote app-cred cloud `cloudferro` →', path)
PY
  unset OS_AUTH_TYPE OS_ACCESS_TOKEN OS_AUTH_URL    # subsequent calls use the cloud
  echo "✓ application credential minted (survives 2FA, non-interactive from here)"
}

# --- teardown (scale to zero) ------------------------------------------------
if [ "${DESTROY:-}" = 1 ]; then
  ensure_appcred
  fip="$(osc server show "$VM_NAME" -f value -c addresses 2>/dev/null | grep -oE '[0-9.]+' | tail -1 || true)"
  osc server delete "$VM_NAME" 2>/dev/null && echo "deleted VM $VM_NAME" || echo "no VM $VM_NAME"
  [ -n "$fip" ] && osc floating ip delete "$fip" 2>/dev/null && echo "released $fip" || true
  exit 0
fi

# --- 2. provision ------------------------------------------------------------
ensure_appcred

# keypair — upload the local public key (generate one if absent).
[ -f "$KEYFILE" ] || ssh-keygen -t ed25519 -N '' -f "$KEYFILE"
osc keypair show "$KEYPAIR" >/dev/null 2>&1 || \
  osc keypair create --public-key "$KEYFILE.pub" "$KEYPAIR" >/dev/null && echo "✓ keypair $KEYPAIR"

# security group — ssh in, everything out.
if ! osc security group show "$SECGROUP" >/dev/null 2>&1; then
  osc security group create "$SECGROUP" >/dev/null
  osc security group rule create --proto tcp --dst-port 22 --remote-ip 0.0.0.0/0 "$SECGROUP" >/dev/null
  echo "✓ security group $SECGROUP (ssh)"
fi

# tenant network for the VM's primary nic (the eodata net is added as a 2nd nic).
if [ -z "$PRIVATE_NET" ]; then
  PRIVATE_NET=s2-flares-net
  if ! osc network show "$PRIVATE_NET" >/dev/null 2>&1; then
    osc network create "$PRIVATE_NET" >/dev/null
    osc subnet create --network "$PRIVATE_NET" --subnet-range 10.0.42.0/24 --dns-nameserver 8.8.8.8 s2-flares-subnet >/dev/null
    osc router create s2-flares-router >/dev/null
    osc router set --external-gateway "$EXTERNAL_NET" s2-flares-router >/dev/null
    osc router add subnet s2-flares-router s2-flares-subnet >/dev/null
    echo "✓ tenant network $PRIVATE_NET + router"
  fi
fi

# cloud-init: install the runtime + clone the repo. eodata reads are anonymous
# from inside WAW3-2 via data.cloudferro.com (CLOUDFERRO/PUBLIC), so cf-run needs
# no secrets — just the right GDAL endpoint env, written to /etc/profile.d.
CLOUD_INIT="$(cat <<'YAML'
#cloud-config
package_update: true
packages: [git, gdal-bin, build-essential, ca-certificates, curl, unzip]
runcmd:
  - curl -fsSL https://deb.nodesource.com/setup_22.x | bash -
  - apt-get install -y nodejs
  - curl -fsSL https://install.duckdb.org | bash || true
  - su - ubuntu -c 'git clone REPO_URL s2-flares && cd s2-flares && npm install gdal-async'
  - |
    cat >/etc/profile.d/eodata.sh <<'ENV'
    export AWS_S3_ENDPOINT=data.cloudferro.com
    export AWS_ACCESS_KEY_ID=CLOUDFERRO
    export AWS_SECRET_ACCESS_KEY=PUBLIC
    export AWS_VIRTUAL_HOSTING=FALSE
    export AWS_HTTPS=YES
    ENV
YAML
)"
CLOUD_INIT="${CLOUD_INIT/REPO_URL/$REPO}"
INIT_FILE="$(mktemp)"; printf '%s\n' "$CLOUD_INIT" >"$INIT_FILE"

# boot — primary nic on the tenant net, second nic on the eodata provider net.
if ! osc server show "$VM_NAME" >/dev/null 2>&1; then
  echo "booting $VM_NAME ($FLAVOR) on $OS_REGION_NAME …"
  osc server create "$VM_NAME" \
    --flavor "$FLAVOR" --image "$IMAGE" --key-name "$KEYPAIR" \
    --security-group "$SECGROUP" \
    --nic net-id="$(osc network show "$PRIVATE_NET" -f value -c id)" \
    --nic net-id="$(osc network show "$EODATA_NET" -f value -c id)" \
    --user-data "$INIT_FILE" --wait >/dev/null
  echo "✓ VM $VM_NAME booted"
fi
rm -f "$INIT_FILE"

# floating ip — allocate + associate if the VM has none.
FIP="$(osc server show "$VM_NAME" -f value -c addresses | grep -oE '[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+' | tail -1 || true)"
if [ -z "$FIP" ]; then
  FIP="$(osc floating ip create "$EXTERNAL_NET" -f value -c floating_ip_address)"
  osc server add floating ip "$VM_NAME" "$FIP"
  echo "✓ floating ip $FIP"
fi

cat <<EOF

provisioned. once cloud-init finishes (~3 min):
  ssh -i $KEYFILE ubuntu@$FIP
  cd s2-flares
  node cf-run.js --aoi aoi/lng-terminals.geojson --preset loose \\
       --start 2025-01-01 --end 2025-12-31 --out runs/lng --concurrency 4
  # eodata reads are free in-region; scale to zero with: DESTROY=1 bash cloudferro/provision.sh
EOF
