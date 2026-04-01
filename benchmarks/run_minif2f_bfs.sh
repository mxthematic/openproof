#!/bin/bash
# Run pure BFS tactic search benchmark on miniF2F.
# Bypasses the LLM entirely -- writes the .lean file directly and runs tactic search.
# Usage: ./benchmarks/run_minif2f_bfs.sh [timeout_secs] [max_problems] [start_from]

set -euo pipefail
cd "$(dirname "$0")/.."

TIMEOUT=${1:-120}
MAX=${2:-0}
START=${3:-0}
RESULTS_DIR="benchmarks/results/bfs_$(date +%Y%m%d_%H%M%S)"
mkdir -p "$RESULTS_DIR"

MINIF2F="benchmarks/miniF2F-lean4/MiniF2F/Test"
if [ ! -d "$MINIF2F" ]; then
    echo "miniF2F not found. Run: git clone https://github.com/yangky11/miniF2F-lean4.git benchmarks/miniF2F-lean4"
    exit 1
fi

PROBLEMS=$(find "$MINIF2F" -name "*.lean" | sort)
TOTAL=$(echo "$PROBLEMS" | wc -l | tr -d ' ')
echo "Found $TOTAL test problems (timeout=${TIMEOUT}s per problem, pure BFS)"

if [ "$START" -gt 0 ]; then
    PROBLEMS=$(echo "$PROBLEMS" | tail -n +$((START + 1)))
fi
if [ "$MAX" -gt 0 ]; then
    PROBLEMS=$(echo "$PROBLEMS" | head -n "$MAX")
    TOTAL=$MAX
fi

SOLVED=0
FAILED=0
ERRORED=0
IDX=0

for PROBLEM_FILE in $PROBLEMS; do
    IDX=$((IDX + 1))
    PROBLEM_NAME=$(basename "$PROBLEM_FILE" .lean)
    LEAN_CONTENT=$(cat "$PROBLEM_FILE")

    if ! echo "$LEAN_CONTENT" | grep -q "sorry"; then
        echo "[$IDX/$TOTAL] $PROBLEM_NAME ... SKIP (no sorry)"
        continue
    fi

    echo -n "[$IDX/$TOTAL] $PROBLEM_NAME ... "

    START_TIME=$(date +%s)
    LOG="$RESULTS_DIR/${PROBLEM_NAME}.log"

    # Run with BFS strategy and pass the raw Lean file content as the problem.
    OPENPROOF_SEARCH_STRATEGY=bfs OPENPROOF_TACTIC_PROPOSER="${OPENPROOF_TACTIC_PROPOSER:-mlx}" \
      timeout "$TIMEOUT" ./target/release/openproof run --problem "$LEAN_CONTENT" > "$LOG" 2>&1 || true
    END_TIME=$(date +%s)
    ELAPSED=$((END_TIME - START_TIME))

    if grep -q "All proof nodes verified\|All nodes verified\|DIRECT VERIFICATION SUCCEEDED\|BFS SOLVED" "$LOG" 2>/dev/null; then
        echo "SOLVED (${ELAPSED}s)"
        SOLVED=$((SOLVED + 1))
        echo "SOLVED $ELAPSED $PROBLEM_NAME" >> "$RESULTS_DIR/summary.txt"
    elif [ "$ELAPSED" -ge "$TIMEOUT" ]; then
        echo "TIMEOUT (${ELAPSED}s)"
        ERRORED=$((ERRORED + 1))
        echo "TIMEOUT $ELAPSED $PROBLEM_NAME" >> "$RESULTS_DIR/summary.txt"
    else
        echo "FAILED (${ELAPSED}s)"
        FAILED=$((FAILED + 1))
        echo "FAILED $ELAPSED $PROBLEM_NAME" >> "$RESULTS_DIR/summary.txt"
    fi

    ATTEMPTED=$((SOLVED + FAILED + ERRORED))
    if [ "$ATTEMPTED" -gt 0 ]; then
        PCT=$((SOLVED * 100 / ATTEMPTED))
        echo "  Running: $SOLVED/$ATTEMPTED ($PCT%) solved"
    fi
done

echo ""
echo "==============================="
echo "  miniF2F BFS Benchmark Results"
echo "==============================="
echo "Solved:  $SOLVED / $((SOLVED + FAILED + ERRORED))"
echo "Failed:  $FAILED"
echo "Timeout: $ERRORED"
if [ "$((SOLVED + FAILED + ERRORED))" -gt 0 ]; then
    echo "Rate:    $((SOLVED * 100 / (SOLVED + FAILED + ERRORED)))%"
fi
echo "Results: $RESULTS_DIR/"
echo "==============================="
