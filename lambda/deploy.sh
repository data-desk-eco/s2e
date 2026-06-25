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
HANDLER="lambda/handler.handler"
# More memory => more vCPU + network bandwidth; the detector is byte-range-read
# bound, so this cuts wall-clock on heavy interior tiles (cost is GB-seconds and
# duration drops roughly in proportion, so it is ~cost-neutral).
MEMORY="${MEMORY:-3008}"
TIMEOUT="${TIMEOUT:-600}"
S3_BUCKET="${S3_BUCKET:-s2-flares-$(aws sts get-caller-identity --query Account --output text)}"
S3_PREFIX="${S3_PREFIX:-s2}"
ENV_VARS="Variables={S2_BUCKET=${S3_BUCKET},S2_PREFIX=${S3_PREFIX}}"

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
cp "$PROJECT_DIR/lambda/handler.js" "$BUILD_DIR/lambda/"

# Install only geotiff. AWS SDK v3 (@aws-sdk/client-s3) is provided by the
# nodejs22.x runtime, so it is not bundled and the zip stays small.
cd "$BUILD_DIR"
cat > package.json <<'PKGJSON'
{ "type": "module", "dependencies": { "geotiff": "^2.1.3" } }
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

# Let the function write per-scene CSVs to the detections bucket (idempotent).
echo "=== Granting S3 write on $S3_BUCKET ==="
aws iam put-role-policy --role-name "$ROLE_NAME" --policy-name s2-write \
    --policy-document '{
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Action": "s3:PutObject",
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

echo ""
echo "=== Deployed ==="
echo "Function: $FUNCTION_NAME ($REGION, ${MEMORY}MB, ${TIMEOUT}s, arm64)"
echo "Output:   s3://${S3_BUCKET}/${S3_PREFIX}/"
