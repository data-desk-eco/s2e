#!/usr/bin/env bash
# cloudferro's official 2FA openrc, vendored verbatim. `source` it to get an
# authenticated openstack session: it prompts for the account password + a TOTP
# code, does the keycloak ropc grant, exchanges it for a scoped keystone token,
# and exports OS_TOKEN / OS_AUTH_TYPE=v3token. the token is valid for hours —
# long enough for a provisioning session. needs `jq`.
for var in $(env | sed -n 's/^\(OS.*\)=.*/\1/p'); do unset "$var"; done
if ! [ -x "$(command -v jq)" ]; then
    echo "jq could not be found in the path. Please install before sourcing rc file"
    return
fi
export OS_AUTH_URL=https://keystone.cloudferro.com/v3
export OS_INTERFACE=public
export OS_IDENTITY_API_VERSION=3
export OS_USERNAME="louis@datadesk.eco"
export OS_REGION_NAME="WAW3-2"
export OS_PROJECT_ID=87f09f10653f4db9bd806c7f430f8962
export OS_PROJECT_NAME="s2-flares"
export OS_PROJECT_DOMAIN_ID="dc1b613a62c04f6eadb143360451cc18"
if [ -z "$OS_USER_DOMAIN_NAME" ]; then unset OS_USER_DOMAIN_NAME; fi
if [ -z "$OS_PROJECT_DOMAIN_ID" ]; then unset OS_PROJECT_DOMAIN_ID; fi
export OS_CLIENT_ID=openstack
export OS_CLIENT_SECRET=KVOWUhVvhjqqUpCdru4IghTHqmMkBt7S
export OS_PROTOCOL=openid
export OS_IDENTITY_PROVIDER=ident_cloudferro-cloud_provider
export OS_AUTH_TYPE=v3oidcaccesstoken
echo "Please enter your OpenStack Password for project $OS_PROJECT_NAME as user $OS_USERNAME: "
read -sr OS_PASSWORD_INPUT
export OS_PASSWORD=$OS_PASSWORD_INPUT
echo "Please enter One-Time Password from your Authenticator App: "
read -r OS_TOTP_INPUT
export KEYCLOAK_TOKEN_ENDPOINT="https://identity.cloudferro.com/auth/realms/CloudFerro-Cloud/protocol/openid-connect/token"
IFS=$'\n'; KEYCLOAK_RESPONSE=$(curl -s -w '\n%{http_code}' -X POST "$KEYCLOAK_TOKEN_ENDPOINT" -H "Content-Type: application/x-www-form-urlencoded" --data-urlencode "password=$OS_PASSWORD" --data-urlencode "username=$OS_USERNAME" -d "grant_type=password&totp=$OS_TOTP_INPUT&client_id=$OS_CLIENT_ID&client_secret=$OS_CLIENT_SECRET")
RESP_WITH_CODE=(); while read -r; do RESP_WITH_CODE+=("$REPLY"); done <<<"$KEYCLOAK_RESPONSE";
RESP=${RESP_WITH_CODE[@]:0:1}
RESP_CODE=${RESP_WITH_CODE[@]:1:1}
if [ "$RESP_CODE" -ne 200 ]; then
    echo -e "Call to Keycloak failed with code $RESP_CODE and message \n $( echo "$RESP" | jq -r )"
    return
fi
TOKEN=$( echo "$RESP" | jq -r '.access_token')
export OS_ACCESS_TOKEN="$TOKEN"
KEYSTONE_TOKEN=$(openstack token issue -f value -c id)
export OS_TOKEN=${KEYSTONE_TOKEN}
export OS_AUTH_TYPE=v3token
