#!/bin/bash

set -euo pipefail

SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" &> /dev/null && pwd)
API_DIR="$SCRIPT_DIR/../capsule-api"
SDK_DIR="$SCRIPT_DIR/../capsule-sdk"

cd "$API_DIR"

echo "Generating OpenAPI 3.1 spec..."
cargo run --bin gen_openapi --features full -- ./openapi.json

if [ ! -s "./openapi.json" ]; then
    echo "Error: ./openapi.json was not generated or is empty." >&2
    exit 1
fi

# Downgrading to OpenAPI 3.0...

npx @apiture/openapi-down-convert --input ./openapi.json > $SDK_DIR/openapi.json
