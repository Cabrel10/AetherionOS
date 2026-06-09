#!/usr/bin/env bash
set -euo pipefail

# Helper to print a section header
section() {
    echo
    echo "===== $1 ====="
}

# 1. Check if Ray or a training Python process is running
section "Ray / Training Process Check"
ps aux | grep -E "python.*train|ray" | grep -v grep || echo "No matching processes found"

# 2. Server stability (RAM, CPU, OOM/Kill messages)
section "Memory / CPU Usage"
free -h

section "Uptime"
uptime

section "Recent OOM / Kill / Error messages (last 5)"
dmesg -T | grep -i -E "oom|kill|error" | tail -n 5 || echo "No recent OOM/Kill/Error messages"

# 3. Latest training log (last 50 lines)
section "Latest Training Log (last 50 lines)"
LATEST_LOG=$(ls -t /home/ubuntu/webapp/MORNINGSTAR/ADAN0/logs/training/*.log 2>/dev/null | head -1 || true)
if [[ -n "$LATEST_LOG" ]]; then
    echo "Log used : $LATEST_LOG"
    tail -n 50 "$LATEST_LOG"
else
    echo "No training logs found."
fi

# 4. Check for saved model checkpoints in the last 2 hours
section "Saved Checkpoints (last 120 minutes)"
find /home/ubuntu/webapp/MORNINGSTAR/ADAN0/logs/checkpoints \
    -type f \( -name "*.zip" -o -name "*.pkl" \) -mmin -120 \
    -printf "%p (%T@)\n" || echo "No recent checkpoint files."

# 5. Cause of possible stop (error lines, exit code)
section "Possible Stop Cause"
STOP_LOG="/mnt/new_data/adan_logs/training/production_run.log"
if [[ -f "$STOP_LOG" ]]; then
    tail -n 50 "$STOP_LOG" | grep -iE "error|failed|GCS|killed|exit|terminated|timeout" || echo "No explicit error lines in last 50 lines."
else
    echo "Stop log not found at $STOP_LOG"
fi

echo -n "Exit code of the last training process (if still alive, will be 0): "
ps -o pid= -C python -o stat= | grep -q . && echo "process still running" || echo "$?"

# 6. Steps performed & final metrics
section "Steps & Performance Metrics"
if [[ -f "$STOP_LOG" ]]; then
    echo "Last known steps / portfolio value:"
    grep -E "TERMINATION CHECK|Portfolio value" "$STOP_LOG" | tail -5
    echo
    echo "Final worker metrics:"
    grep -E "METRICS_SYNC|EPISODE_END_STATS|realized_pnl|Sharpe" "$STOP_LOG" | tail -20
    echo
    echo "Last risk tier update:"
    grep -E "RISK_UPDATE.*Palier" "$STOP_LOG" | tail -5
else
    echo "Log not found for metrics extraction."
fi

# 7. Critical model.zip / checkpoint files
section "Critical model.zip / checkpoint files"
find /mnt/new_data/adan_logs/checkpoints -name "model.zip" 2>/dev/null | head -20
find /mnt/new_data/adan_logs/checkpoints -name "vecnormalize.pkl" 2>/dev/null | head -20
find . -name "model.zip" 2>/dev/null | head -20
find . -name "checkpoint_*" -type d 2>/dev/null | head -10
echo
echo "List of files in the main checkpoint directory (if any):"
ls -lah /mnt/new_data/adan_logs/checkpoints/adan_pbt_training/ 2>/dev/null || echo "Directory not found."
find /mnt/new_data/adan_logs/checkpoints -type f | sort | head -30

# 8. Ray loopback configuration verification
section "Ray Loopback Configuration"
if [[ -f "$STOP_LOG" ]]; then
    head -n 50 "$STOP_LOG" | grep -E "127\.0\.0\.1|node_ip|loopback|RAY_NODE" || echo "No loopback indicators found in log."
else
    echo "Log not found for Ray config check."
fi

echo "Ray node IP(s) from experiment_state JSON (if present):"
grep "node_ip" /mnt/new_data/adan_logs/checkpoints/adan_pbt_training/experiment_state-*.json 2>/dev/null | head -3 || echo "No JSON files with node_ip found."

# 9. Real duration & stability
section "Run Duration & Stability"
if [[ -f "$STOP_LOG" ]]; then
    echo "First timestamps (first 5 lines):"
    head -n 5 "$STOP_LOG" | grep "2026"
    echo
    echo "Last timestamps (last 5 lines):"
    tail -n 5 "$STOP_LOG" | grep "2026"
    echo
    echo "Number of initializations (possible Ray restarts):"
    grep -c "ADAN Trading Bot.*initializ" "$STOP_LOG" || echo "0"
else
    echo "Log not found for duration/stability analysis."
fi
