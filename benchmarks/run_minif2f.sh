#!/bin/bash
# Run OpenProof against miniF2F-test problems.
# Usage: ./run_minif2f.sh [timeout_per_problem_seconds] [max_problems]

set -euo pipefail

TIMEOUT=${1:-300}  # 5 min per problem default
MAX=${2:-0}        # 0 = all problems
RESULTS_DIR="benchmarks/results/$(date +%Y%m%d_%H%M%S)"
mkdir -p "$RESULTS_DIR"

# Check if miniF2F is downloaded
MINIF2F_DIR="benchmarks/miniF2F-lean4"
if [ ! -d "$MINIF2F_DIR" ]; then
    echo "Downloading miniF2F Lean 4 problems..."
    git clone https://github.com/openai/miniF2F.git "$MINIF2F_DIR" 2>/dev/null || {
        echo "Failed to clone miniF2F. Check https://github.com/openai/miniF2F"
        exit 1
    }
fi

# Find test problems (Lean 4 format)
PROBLEMS=$(find "$MINIF2F_DIR" -name "*.lean" -path "*/test/*" | sort)
TOTAL=$(echo "$PROBLEMS" | wc -l | tr -d ' ')
echo "Found $TOTAL test problems"

if [ "$MAX" -gt 0 ] && [ "$MAX" -lt "$TOTAL" ]; then
    PROBLEMS=$(echo "$PROBLEMS" | head -n "$MAX")
    TOTAL=$MAX
fi

SOLVED=0
FAILED=0
ERRORED=0

for PROBLEM_FILE in $PROBLEMS; do
    PROBLEM_NAME=$(basename "$PROBLEM_FILE" .lean)
    echo -n "[$((SOLVED + FAILED + ERRORED + 1))/$TOTAL] $PROBLEM_NAME ... "

    # Extract the theorem statement from the file
    STATEMENT=$(grep -m1 "theorem\|lemma" "$PROBLEM_FILE" | sed 's/^.*theorem /theorem /; s/^.*lemma /lemma /' | head -1)
    if [ -z "$STATEMENT" ]; then
        echo "SKIP (no theorem found)"
        continue
    fi

    # Run OpenProof headless with timeout
    START=$(date +%s)
    RESULT=$(timeout "$TIMEOUT" cargo run -q -- run --problem "Prove: $STATEMENT" 2>"$RESULTS_DIR/${PROBLEM_NAME}.log" || echo "TIMEOUT_OR_ERROR")
    END=$(date +%s)
    ELAPSED=$((END - START))

    # Check if verification succeeded
    if grep -q "All nodes verified\|VERIFIED" "$RESULTS_DIR/${PROBLEM_NAME}.log" 2>/dev/null; then
        echo "SOLVED (${ELAPSED}s)"
        SOLVED=$((SOLVED + 1))
        echo "$PROBLEM_NAME SOLVED $ELAPSED" >> "$RESULTS_DIR/summary.txt"
    elif echo "$RESULT" | grep -q "TIMEOUT_OR_ERROR"; then
        echo "TIMEOUT (${ELAPSED}s)"
        ERRORED=$((ERRORED + 1))
        echo "$PROBLEM_NAME TIMEOUT $ELAPSED" >> "$RESULTS_DIR/summary.txt"
    else
        echo "FAILED (${ELAPSED}s)"
        FAILED=$((FAILED + 1))
        echo "$PROBLEM_NAME FAILED $ELAPSED" >> "$RESULTS_DIR/summary.txt"
    fi
done

echo ""
echo "=== Results ==="
echo "Solved: $SOLVED / $TOTAL ($(( SOLVED * 100 / TOTAL ))%)"
echo "Failed: $FAILED"
echo "Timeout/Error: $ERRORED"
echo "Results saved to: $RESULTS_DIR/"
