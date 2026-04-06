#!/bin/bash
set -euo pipefail

# Deploy s2-flares Lambda function to us-west-2 (co-located with S2 COGs on S3)

FUNCTION_NAME="s2-flares-detect"
REGION="us-west-2"
RUNTIME="nodejs22.x"
HANDLER="lambda/handler.handler"
MEMORY=2048
TIMEOUT=300
ROLE_NAME="s2-flares-lambda-role"

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"
BUILD_DIR=$(mktemp -d)

echo "=== Building deployment package ==="

# Copy source files (only what's needed)
mkdir -p "$BUILD_DIR/lib/vendor" "$BUILD_DIR/lambda"
for f in cog.js detect.js geo.js stac.js cluster.js index.js; do
    cp "$PROJECT_DIR/lib/$f" "$BUILD_DIR/lib/"
done
cp "$PROJECT_DIR/lib/vendor/geotiff-esm.js" "$BUILD_DIR/lib/vendor/"
# Note: vendored geotiff.js is NOT needed — Node.js uses npm geotiff package
cp "$PROJECT_DIR/lambda/handler.js" "$BUILD_DIR/lambda/"

# Install only geotiff (Lambda doesn't need @aws-sdk/client-lambda)
cd "$BUILD_DIR"
cat > package.json <<'PKGJSON'
{ "type": "module", "dependencies": { "geotiff": "^2.1.3" } }
PKGJSON
npm install --omit=dev 2>&1 | tail -3

# Create zip
echo "=== Creating zip ==="
zip -qr "$PROJECT_DIR/lambda/deploy.zip" . -x '*.git*'
echo "Package size: $(du -h "$PROJECT_DIR/lambda/deploy.zip" | cut -f1)"

cd "$PROJECT_DIR"
rm -rf "$BUILD_DIR"

# --- IAM Role ---

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

    aws iam attach-role-policy \
        --role-name "$ROLE_NAME" \
        --policy-arn "arn:aws:iam::aws:policy/service-role/AWSLambdaBasicExecutionRole"

    echo "Waiting for role propagation..."
    sleep 10
else
    echo "Using existing role: $ROLE_ARN"
fi

# --- Lambda Function ---

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
        --query 'FunctionArn' --output text
else
    echo "Updating function: $FUNCTION_NAME"
    aws lambda update-function-code \
        --function-name "$FUNCTION_NAME" \
        --region "$REGION" \
        --zip-file "fileb://lambda/deploy.zip" \
        --query 'FunctionArn' --output text

    # Wait for update to complete before updating config
    aws lambda wait function-updated --function-name "$FUNCTION_NAME" --region "$REGION"

    # --architectures is fixed at create time and rejected by update-function-configuration.
    aws lambda update-function-configuration \
        --function-name "$FUNCTION_NAME" \
        --region "$REGION" \
        --runtime "$RUNTIME" \
        --handler "$HANDLER" \
        --memory-size "$MEMORY" \
        --timeout "$TIMEOUT" \
        --query 'FunctionArn' --output text
fi

echo ""
echo "=== Deployed ==="
echo "Function: $FUNCTION_NAME"
echo "Region:   $REGION"
echo "Runtime:  $RUNTIME (ARM64/Graviton)"
echo "Memory:   ${MEMORY}MB"
echo "Timeout:  ${TIMEOUT}s"
