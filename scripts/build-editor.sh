#!/bin/bash
set -euo pipefail
cd "$(dirname "$0")/../crates/openproof-dashboard/editor"
echo "Installing editor dependencies..."
npm ci
echo "Building editor bundle..."
npm run build
echo "Editor bundle built to static/editor-dist/"
