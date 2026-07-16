# the central datadesk data store: one public cloudferro bucket every repo
# contributes its own prefix to. sourced by box.sh and by the sibling repos'
# upload scripts (burnoff vnf/, firedamp plumes/, ch4id features/).
#
#   detections/mgrs=*/data.parquet   s2-flares  (public)
#   clusters/…                       s2-flares  (public, derived view)
#   clouds/…                         s2-flares  (PRIVATE build artifact)
#   coverage.geojson                 s2-flares  (public)
#   vnf/data.parquet                 burnoff    (public)
#   plumes/data.parquet              firedamp   (public)
#   features/data.fgb                ch4id      (public)
#
# bucket-level config (public-read policy + cors) is owned here: box.sh publish.
: "${STORE_REGION:=WAW3-2}"
: "${STORE_BUCKET:=datadesk-archive}"
STORE_ENDPOINT="https://s3.$STORE_REGION.cloudferro.com"
STORE_URL="$STORE_ENDPOINT/$STORE_BUCKET"

# export AWS_ACCESS_KEY_ID/AWS_SECRET_ACCESS_KEY for the store. static env keys
# win (ci); else mint/reuse openstack ec2 creds, authenticating via the vendored
# 2fa openrc (non-interactive when .env sets CLOUDFERRO_PASSWORD/TOTP_SECRET).
store_creds() {
    [ -n "${AWS_ACCESS_KEY_ID:-}" ] && [ -n "${AWS_SECRET_ACCESS_KEY:-}" ] && return 0
    local here; here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
    if ! openstack token issue >/dev/null 2>&1; then
        [ -f "$here/../.env" ] && . "$here/../.env"
        set +eu   # the vendored openrc is written for a lax shell
        if [ -n "${CLOUDFERRO_TOTP_SECRET:-}" ] && command -v oathtool >/dev/null; then
            source "$here/s2-flares-openrc-2fa.sh" >/dev/null \
                < <(printf '%s\n%s\n' "${CLOUDFERRO_PASSWORD:-}" "$(oathtool -b --totp "$CLOUDFERRO_TOTP_SECRET")")
        else
            source "$here/s2-flares-openrc-2fa.sh"
        fi
        set -eu; unset IFS
    fi
    local c; c=$(openstack ec2 credentials list -f value -c Access -c Secret 2>/dev/null | head -1)
    if [ -z "$c" ] || [ "$c" = "null null" ]; then
        openstack ec2 credentials create >/dev/null
        c=$(openstack ec2 credentials list -f value -c Access -c Secret | head -1)
    fi
    export AWS_ACCESS_KEY_ID=${c%% *} AWS_SECRET_ACCESS_KEY=${c##* }
    [ -n "$AWS_ACCESS_KEY_ID" ] && [ -n "$AWS_SECRET_ACCESS_KEY" ]
}
