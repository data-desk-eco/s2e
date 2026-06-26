#!/bin/bash
set -euo pipefail

# Deploy the s2-flares detection Lambda to us-west-2 (co-located with the public
# sentinel-2-l2a COG bucket, so reads are in-region). Idempotent: creates the
# function + IAM role + detections bucket on first run, updates code/config after.
# All names are env-overridable so a consumer (e.g. permian-flaring) can deploy
# the same code under its own function/bucket without forking this script.

FUNCTION_NAME="${FUNCTION_NAME:-s2-flares-detect}"
REGION="${REGION:-us-west-2}"
ROLE_NAME="${ROLE_NAME:-s2-flares-lambda-role}"
RUNTIME="nodejs22.x"
# Override HANDLER to deploy the web API instead of the per-scene bulk shell:
#   FUNCTION_NAME=s2-flares-api HANDLER=lambda/api.handler PUBLIC_URL=1 bash lambda/deploy.sh
HANDLER="${HANDLER:-lambda/handler.handler}"
# More memory => more vCPU + network bandwidth; the detector is byte-range-read
# bound, so this cuts wall-clock on heavy interior tiles (cost is GB-seconds and
# duration drops roughly in proportion, so it is ~cost-neutral).
MEMORY="${MEMORY:-3008}"
TIMEOUT="${TIMEOUT:-600}"
S3_BUCKET="${S3_BUCKET:-s2-flares-$(aws sts get-caller-identity --query Account --output text)}"
S3_PREFIX="${S3_PREFIX:-s2}"
# Web-API guardrails (ignored by the per-scene handler). MAX_AOI_KM2 is the hard
# area cap; MAX_CONCURRENCY caps a public URL's blast radius (-1 = unreserved).
MAX_AOI_KM2="${MAX_AOI_KM2:-2500}"
DEFAULT_DAYS="${DEFAULT_DAYS:-90}"
MAX_CONCURRENCY="${MAX_CONCURRENCY:-10}"
# CACHE_PREFIX namespaces the web API's per-scene parquet cache, kept apart from
# the bulk handler's CSV prefix (S2_PREFIX) so the two collections never mix.
CACHE_PREFIX="${CACHE_PREFIX:-flares}"
ENV_VARS="Variables={S2_BUCKET=${S3_BUCKET},S2_PREFIX=${S3_PREFIX},CACHE_PREFIX=${CACHE_PREFIX},MAX_AOI_KM2=${MAX_AOI_KM2},DEFAULT_DAYS=${DEFAULT_DAYS}}"

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"
BUILD_DIR=$(mktemp -d)

echo "=== Building deployment package ==="

# Copy only what the handler imports (lib/ core + the handler), preserving the
# ../lib relative-import structure the handler relies on.
mkdir -p "$BUILD_DIR/lib/vendor" "$BUILD_DIR/lambda"
for f in cog.js coverage.js detect.js geo.js stac.js cluster.js score.js index.js; do
    cp "$PROJECT_DIR/lib/$f" "$BUILD_DIR/lib/"
done
cp "$PROJECT_DIR/lib/vendor/geotiff-esm.js" "$BUILD_DIR/lib/vendor/"
# Note: vendored geotiff.js (UMD) is NOT needed — Node uses the npm geotiff package.
# run.js is the shared AOI pipeline (api.handler); both handlers ship in every zip.
cp "$PROJECT_DIR/lib/run.js" "$BUILD_DIR/lib/"
# scene-store.js is the web API's per-scene parquet cache (api.handler).
cp "$PROJECT_DIR/lambda/handler.js" "$PROJECT_DIR/lambda/api.js" \
   "$PROJECT_DIR/lambda/scene-store.js" "$BUILD_DIR/lambda/"

# Bundle geotiff (COG reads) + hyparquet/-writer (the scene cache's parquet I/O).
# AWS SDK v3 (@aws-sdk/client-s3) is provided by the nodejs22.x runtime, so it is
# not bundled and the zip stays small.
cd "$BUILD_DIR"
cat > package.json <<'PKGJSON'
{ "type": "module", "dependencies": { "geotiff": "^2.1.3", "hyparquet": "^1.26.1", "hyparquet-writer": "^0.16.1" } }
PKGJSON
npm install --omit=dev 2>&1 | tail -3

echo "=== Creating zip ==="
zip -qr "$PROJECT_DIR/lambda/deploy.zip" . -x '*.git*'
echo "Package size: $(du -h "$PROJECT_DIR/lambda/deploy.zip" | cut -f1)"

cd "$PROJECT_DIR"
rm -rf "$BUILD_DIR"

# --- IAM role ---

echo "=== Setting up IAM role ==="
ROLE_ARN=$(aws iam get-role --role-name "$ROLE_NAME" --query 'Role.Arn' --output text 2>/dev/null || true)

if [ -z "$ROLE_ARN" ] || [ "$ROLE_ARN" = "None" ]; then
    echo "Creating IAM role: $ROLE_NAME"
    ROLE_ARN=$(aws iam create-role \
        --role-name "$ROLE_NAME" \
        --assume-role-policy-document '{
            "Version": "2012-10-17",
            "Statement": [{
                "Effect": "Allow",
                "Principal": { "Service": "lambda.amazonaws.com" },
                "Action": "sts:AssumeRole"
            }]
        }' \
        --query 'Role.Arn' --output text)
    aws iam attach-role-policy --role-name "$ROLE_NAME" \
        --policy-arn "arn:aws:iam::aws:policy/service-role/AWSLambdaBasicExecutionRole"
    echo "Waiting for role propagation..."
    sleep 10
else
    echo "Using existing role: $ROLE_ARN"
fi

# Let the function read + write objects in the detections bucket (idempotent).
# GetObject is what the web API's per-scene parquet cache needs to serve a hit.
echo "=== Granting S3 read/write on $S3_BUCKET ==="
aws iam put-role-policy --role-name "$ROLE_NAME" --policy-name s2-write \
    --policy-document '{
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Action": ["s3:PutObject", "s3:GetObject"],
            "Resource": "arn:aws:s3:::'"${S3_BUCKET}"'/*"
        }]
    }'

# Create the detections bucket if needed (us-west-2 needs LocationConstraint;
# us-east-1 must omit it).
if ! aws s3api head-bucket --bucket "$S3_BUCKET" >/dev/null 2>&1; then
    echo "=== Creating bucket $S3_BUCKET ==="
    if [ "$REGION" = "us-east-1" ]; then
        aws s3api create-bucket --bucket "$S3_BUCKET" --region "$REGION" >/dev/null
    else
        aws s3api create-bucket --bucket "$S3_BUCKET" --region "$REGION" \
            --create-bucket-configuration "LocationConstraint=$REGION" >/dev/null
    fi
fi

# --- Lambda function ---

echo "=== Deploying Lambda function ==="
EXISTING=$(aws lambda get-function --function-name "$FUNCTION_NAME" --region "$REGION" 2>/dev/null || true)

if [ -z "$EXISTING" ]; then
    echo "Creating function: $FUNCTION_NAME"
    aws lambda create-function \
        --function-name "$FUNCTION_NAME" \
        --region "$REGION" \
        --runtime "$RUNTIME" \
        --handler "$HANDLER" \
        --role "$ROLE_ARN" \
        --memory-size "$MEMORY" \
        --timeout "$TIMEOUT" \
        --zip-file "fileb://lambda/deploy.zip" \
        --architectures arm64 \
        --environment "$ENV_VARS" \
        --query 'FunctionArn' --output text
else
    echo "Updating function: $FUNCTION_NAME"
    aws lambda update-function-code \
        --function-name "$FUNCTION_NAME" \
        --region "$REGION" \
        --zip-file "fileb://lambda/deploy.zip" \
        --query 'FunctionArn' --output text
    aws lambda wait function-updated --function-name "$FUNCTION_NAME" --region "$REGION"
    # --architectures is fixed at create time and rejected by update-function-configuration.
    aws lambda update-function-configuration \
        --function-name "$FUNCTION_NAME" \
        --region "$REGION" \
        --runtime "$RUNTIME" \
        --handler "$HANDLER" \
        --memory-size "$MEMORY" \
        --timeout "$TIMEOUT" \
        --environment "$ENV_VARS" \
        --query 'FunctionArn' --output text
fi

aws lambda wait function-updated --function-name "$FUNCTION_NAME" --region "$REGION" 2>/dev/null || true

# --- public web API: streaming Function URL (opt-in) -------------------------
# PUBLIC_URL=1 fronts the function with an HTTPS Function URL in RESPONSE_STREAM
# mode (so api.handler can stream NDJSON), open CORS, public auth, and reserved
# concurrency as a cost/abuse ceiling. No API Gateway — the URL is the whole API.
if [ "${PUBLIC_URL:-}" = "1" ]; then
    echo "=== Configuring public Function URL ==="
    # Array (not a string) so the CORS JSON stays one arg — its [ ] * are bash globs.
    URL_CFG=(--auth-type NONE --invoke-mode RESPONSE_STREAM --cors '{"AllowOrigins":["*"],"AllowMethods":["GET","POST"]}')
    aws lambda create-function-url-config --function-name "$FUNCTION_NAME" --region "$REGION" "${URL_CFG[@]}" >/dev/null 2>&1 \
        || aws lambda update-function-url-config --function-name "$FUNCTION_NAME" --region "$REGION" "${URL_CFG[@]}" >/dev/null
    # Public unauthenticated invoke (idempotent).
    aws lambda add-permission --function-name "$FUNCTION_NAME" --region "$REGION" \
        --statement-id FunctionURLPublic --action lambda:InvokeFunctionUrl \
        --principal '*' --function-url-auth-type NONE >/dev/null 2>&1 || true
    # Cap concurrent executions so a public endpoint can't run away on cost.
    if [ "$MAX_CONCURRENCY" -ge 0 ] 2>/dev/null; then
        aws lambda put-function-concurrency --function-name "$FUNCTION_NAME" --region "$REGION" \
            --reserved-concurrent-executions "$MAX_CONCURRENCY" >/dev/null
    fi
    FUNCTION_URL=$(aws lambda get-function-url-config --function-name "$FUNCTION_NAME" --region "$REGION" \
        --query 'FunctionUrl' --output text)
fi

echo ""
echo "=== Deployed ==="
echo "Function: $FUNCTION_NAME ($REGION, ${MEMORY}MB, ${TIMEOUT}s, arm64)"
echo "Output:   s3://${S3_BUCKET}/${S3_PREFIX}/"
if [ -n "${FUNCTION_URL:-}" ]; then
    echo "Web API:  $FUNCTION_URL (cap ${MAX_AOI_KM2} km², ≤${MAX_CONCURRENCY} concurrent)"
    echo "  curl '${FUNCTION_URL}?bbox=-104,31.5,-103,32.5&stream=1'"
fi
