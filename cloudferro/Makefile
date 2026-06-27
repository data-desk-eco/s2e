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
.ONESHELL:
SHELL := bash
.SHELLFLAGS := -ec

VM        ?= s2-flares
FLAVOR    ?= eo1.large
IMAGE     ?= Ubuntu 22.04 LTS
KEYPAIR   ?= s2-flares
KEYFILE   ?= $(HOME)/.ssh/id_ed25519
SECGROUP  ?= s2-flares
NET       ?= s2-flares-net
EXTNET    ?= external
EODATANET ?= eodata
auth       = source ./s2-flares-openrc-2fa.sh

.PHONY: up ip ssh down
up:
	$(auth)
	[ -f $(KEYFILE) ] || ssh-keygen -t ed25519 -N '' -f $(KEYFILE)
	openstack keypair show $(KEYPAIR) >/dev/null 2>&1 || openstack keypair create --public-key $(KEYFILE).pub $(KEYPAIR)
	openstack security group show $(SECGROUP) >/dev/null 2>&1 || {
	  openstack security group create $(SECGROUP)
	  openstack security group rule create --proto tcp --dst-port 22 --remote-ip 0.0.0.0/0 $(SECGROUP); }
	openstack network show $(NET) >/dev/null 2>&1 || {
	  openstack network create $(NET)
	  openstack subnet create --network $(NET) --subnet-range 10.0.42.0/24 --dns-nameserver 8.8.8.8 $(NET)-sub
	  openstack router create $(NET)-rtr
	  openstack router set --external-gateway $(EXTNET) $(NET)-rtr
	  openstack router add subnet $(NET)-rtr $(NET)-sub; }
	openstack server show $(VM) >/dev/null 2>&1 || openstack server create $(VM) \
	  --flavor $(FLAVOR) --image "$(IMAGE)" --key-name $(KEYPAIR) --security-group $(SECGROUP) \
	  --nic net-id=$$(openstack network show $(NET) -f value -c id) \
	  --nic net-id=$$(openstack network show $(EODATANET) -f value -c id) \
	  --user-data cloud-init.yaml --wait
	port=$$(openstack port list --server $(VM) --network $(NET) -f value -c id | head -1)
	fip=$$(openstack floating ip list --port $$port -f value -c "Floating IP Address" | head -1)
	if [ -z "$$fip" ]; then
	  fip=$$(openstack floating ip create $(EXTNET) -f value -c floating_ip_address)
	  openstack floating ip set --port $$port $$fip
	fi
	echo "$$fip" >.box-ip
	echo "provisioned → ssh -i $(KEYFILE) ubuntu@$$fip  (wait ~3min for cloud-init)"

ip:
	$(auth)
	openstack port list --server $(VM) --network $(NET) -f value -c id | head -1 | \
	  xargs -I{} openstack floating ip list --port {} -f value -c "Floating IP Address" | head -1 | tee .box-ip

ssh:
	ssh -i $(KEYFILE) ubuntu@$$(cat .box-ip)

down:
	$(auth)
	port=$$(openstack port list --server $(VM) --network $(NET) -f value -c id | head -1 || true)
	fip=$$([ -n "$$port" ] && openstack floating ip list --port $$port -f value -c "Floating IP Address" | head -1 || true)
	openstack server delete $(VM) && echo "deleted $(VM)" || echo "no VM $(VM)"
	[ -n "$$fip" ] && openstack floating ip delete $$fip && echo "released $$fip" || true
	rm -f .box-ip
