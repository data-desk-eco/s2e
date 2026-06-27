# EU-sovereign bulk-detection box on CloudFerro WAW3-2, co-located with the
# Copernicus `eodata` archive (free in-region JP2 reads). auth is the vendored
# official 2FA openrc (one password+TOTP prompt per `make` target); everything
# else is plain openstack. the box reads eodata anonymously, so it needs no creds.
#
#   make up     provision: keypair, security group, tenant net, VM on the eodata
#               net + floating IP, cloud-init (node 22 / gdal / duckdb + clone)
#   make ip     print the box's floating IP        make ssh   ssh into the box
#   make down   delete the VM + release its floating IP (scale to zero)
#
# idempotent: re-running `up` reuses any resource that already exists.
#
# each target runs as ONE bash script (`export` the recipe, then `bash -ec
# "$$VAR"`), so the OS_* env the openrc exports reaches every openstack call in
# the body. we deliberately do NOT use .ONESHELL: macOS ships GNU make 3.81,
# which predates it (3.82) and silently ignores it — putting each recipe line in
# its own shell, dropping the sourced auth, and falling back to a password prompt
# that 401s. the single-shell form works on 3.81 too.

VM        ?= s2-flares
FLAVOR    ?= eo1.large
IMAGE     ?= Ubuntu 22.04 LTS
KEYPAIR   ?= s2-flares
KEYFILE   ?= $(HOME)/.ssh/id_ed25519
SECGROUP  ?= s2-flares
NET       ?= s2-flares-net
EXTNET    ?= external
EODATANET ?= eodata
# source the 2fa openrc, then refuse to continue unless it actually minted a
# keystone token (else the openstack calls below silently fall back to password
# auth → a confusing `Password:` prompt and 401).
auth = source ./s2-flares-openrc-2fa.sh; [ -n "$$OS_TOKEN" ] || { echo "auth failed: no keystone token — wrong password/TOTP, or OIDC token-issue rejected" >&2; exit 1; }

define UP
$(auth)
say(){ printf '\n\033[1;36m→ %s\033[0m\n' "$$*"; }
say "Keypair $(KEYPAIR)"
[ -f $(KEYFILE) ] || ssh-keygen -t ed25519 -N '' -f $(KEYFILE) >/dev/null
openstack keypair show $(KEYPAIR) >/dev/null 2>&1 || openstack keypair create --public-key $(KEYFILE).pub $(KEYPAIR) >/dev/null
say "Security group $(SECGROUP)"
openstack security group show $(SECGROUP) >/dev/null 2>&1 || {
  openstack security group create $(SECGROUP) >/dev/null
  openstack security group rule create --proto tcp --dst-port 22 --remote-ip 0.0.0.0/0 $(SECGROUP) >/dev/null; }
say "Network $(NET)"
openstack network show $(NET) >/dev/null 2>&1 || {
  openstack network create $(NET) >/dev/null
  openstack subnet create --network $(NET) --subnet-range 10.0.42.0/24 --dns-nameserver 8.8.8.8 $(NET)-sub >/dev/null
  openstack router create $(NET)-rtr >/dev/null
  openstack router set --external-gateway $(EXTNET) $(NET)-rtr >/dev/null
  openstack router add subnet $(NET)-rtr $(NET)-sub >/dev/null; }
if openstack server show $(VM) >/dev/null 2>&1; then say "VM $(VM) already exists"; else
  say "Booting VM $(VM) ($(FLAVOR)) — a few minutes…"
  openstack server create $(VM) \
    --flavor $(FLAVOR) --image "$(IMAGE)" --key-name $(KEYPAIR) --security-group $(SECGROUP) \
    --nic net-id=$$(openstack network show $(NET) -f value -c id) \
    --nic net-id=$$(openstack network show $(EODATANET) -f value -c id) \
    --user-data cloud-init.yaml --wait >/dev/null
fi
say "Floating IP"
port=$$(openstack port list --server $(VM) --network $(NET) -f value -c id | head -1)
fip=$$(openstack floating ip list --port $$port -f value -c "Floating IP Address" | head -1)
if [ -z "$$fip" ]; then
  fip=$$(openstack floating ip create $(EXTNET) -f value -c floating_ip_address)
  openstack floating ip set --port $$port $$fip >/dev/null
fi
echo "$$fip" >.box-ip
printf '\n\033[1;32m✓ provisioned\033[0m  →  make ssh   (first boot ~3min: node 22 / gdal / clone)\n   ssh -i %s eouser@%s\n' "$(KEYFILE)" "$$fip"
endef

define IP
$(auth)
openstack port list --server $(VM) --network $(NET) -f value -c id | head -1 | \
  xargs -I{} openstack floating ip list --port {} -f value -c "Floating IP Address" | head -1 | tee .box-ip
endef

define DOWN
$(auth)
say(){ printf '\n\033[1;36m→ %s\033[0m\n' "$$*"; }
port=$$(openstack port list --server $(VM) --network $(NET) -f value -c id | head -1 || true)
fip=$$([ -n "$$port" ] && openstack floating ip list --port $$port -f value -c "Floating IP Address" | head -1 || true)
say "Deleting VM $(VM)"
openstack server delete $(VM) --wait >/dev/null 2>&1 && echo "  deleted" || echo "  no VM $(VM)"
if [ -n "$$fip" ]; then say "Releasing floating IP $$fip"; openstack floating ip delete $$fip >/dev/null 2>&1 || true; fi
rm -f .box-ip
printf '\n\033[1;32m✓ scaled to zero\033[0m\n'
endef

export UP IP DOWN
.PHONY: up ip ssh down
up:  ; @bash -ec "$$UP"
ip:  ; @bash -ec "$$IP"
down:; @bash -ec "$$DOWN"
ssh: ; @ssh -i $(KEYFILE) eouser@$$(cat .box-ip)
