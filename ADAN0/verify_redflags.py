#!/usr/bin/env python3
"""
verify_redflags.py – Run a quick sanity‑check on the ADAN0 training and
deterministic back‑test logs to see whether the “red‑flags” identified in the
expert audit are actually justified.

The script is self‑contained and uses only the Python standard library,
so it can be executed directly inside the ADAN0 directory:

    cd ADAN0
    python3 verify_redflags.py

It will:

1. Load the training summary JSON (`logs/training/production_run.log`).
2. Load all metric event JSON‑lines files under `logs/metrics/`.
3. Apply three heuristic checks:
   * Promotion‑Bonus exploitation
   * Time‑Decay pressure
   * Data‑Ghosting (duplicate entry prices across distinct timestamps)
4. Print a concise report and exit with status 0 if no critical red‑flags are
   detected, otherwise exit with status 1.

The default thresholds are deliberately conservative; they can be tuned by
editing the constants near the top of the file.
"""

import json
import sys
from collections import defaultdict
from pathlib import Path
from typing import Dict, List

# --------------------------------------------------------------------------- #
# Tunable thresholds (adjust if you have domain‑specific knowledge)
# --------------------------------------------------------------------------- #

# Promotion‑bonus schedule (example values – replace with your actual bonuses)
BONUS_SCHEDULE = {
    "micro_to_small": 5.0,
    "high_to_enterprise": 40.0,  # note: "enterprise" is the original key; keep it for compatibility
}

# Time‑decay applied per training step (negative value means a penalty)
TIME_DECAY_PER_STEP = -0.01

# Minimum profit multiplier over the largest bonus to flag potential exploitation
PROMOTION_PROFIT_MULTIPLIER = 3.0

# Minimum net profit / cumulative decay ratio to consider the model
# “forced” to trade aggressively because decay dominates.
TIME_DECAY_PROFIT_RATIO = 0.5  # i.e. profit must be at least 50 % of decay

# --------------------------------------------------------------------------- #
# Helper classes
# --------------------------------------------------------------------------- #

class RedFlagReport:
    """Collects findings and renders a human‑readable summary."""
    def __init__(self):
        self.promotions: List[str] = []
        self.time_decay: List[str] = []
        self.ghost_entries: List[str] = []

    def any(self) -> bool:
        return bool(self.promotions or self.time_decay or self.ghost_entries)

    def render(self) -> str:
        lines = ["=== Red‑Flag Verification Report ===\n"]
        if self.promotions:
            lines.append("🚩 Promotion‑Bonus exploitation:")
            lines.extend(f"  - {msg}" for msg in self.promotions)
            lines.append("")
        if self.time_decay:
            lines.append("🚩 Time‑Decay pressure:")
            lines.extend(f"  - {msg}" for msg in self.time_decay)
            lines.append("")
        if self.ghost_entries:
            lines.append("🚩 Data‑Ghosting (duplicate entry prices):")
            lines.extend(f"  - {msg}" for msg in self.ghost_entries)
            lines.append("")
        if not self.any():
            lines.append("✅ No critical red‑flags detected.")
        return "\n".join(lines)


# --------------------------------------------------------------------------- #
# Loading functions
# --------------------------------------------------------------------------- #

def load_training_log() -> Dict:
    """Parse the production_run.log JSON file."""
    log_path = Path("logs/training/production_run.log")
    if not log_path.is_file():
        sys.stderr.write(f"⚠️  Training log not found at {log_path}\n")
        sys.exit(2)

    try:
        with log_path.open("r", encoding="utf-8") as f:
            data = json.load(f)
    except json.JSONDecodeError as exc:
        sys.stderr.write(f"⚠️  Failed to decode training log JSON: {exc}\n")
        sys.exit(2)

    return data


def load_metric_events() -> List[Dict]:
    """Collect every JSON line from the metrics directory."""
    metrics_dir = Path("logs/metrics")
    events: List[Dict] = []

    if not metrics_dir.is_dir():
        # No metric files – the rest of the checks will simply skip this part.
        return events

    for jsonl_file in metrics_dir.rglob("*.jsonl"):
        try:
            with jsonl_file.open("r", encoding="utf-8") as f:
                for line in f:
                    line = line.strip()
                    if not line:
                        continue
                    try:
                        ev = json.loads(line)
                        events.append(ev)
                    except json.JSONDecodeError:
                        # Skip malformed lines – they are not critical for the audit.
                        continue
        except OSError as exc:
            sys.stderr.write(f"⚠️  Could not read {jsonl_file}: {exc}\n")
    return events


# --------------------------------------------------------------------------- #
# Red‑flag detection algorithms
# --------------------------------------------------------------------------- #

def detect_promotion_bonus(training: Dict, events: List[Dict], report: RedFlagReport):
    """
    Heuristic:
      * Compute total realised PnL from trade_closed events.
      * If total PnL exceeds PROMOTION_PROFIT_MULTIPLIER × largest bonus,
        flag a possible promotion‑bonus driven profit.
    """
    if not events:
        return

    total_pnl = sum(
        ev.get("pnl", 0.0)
        for ev in events
        if ev.get("event") == "trade_closed"
    )
    if total_pnl == 0:
        return

    max_bonus = max(BONUS_SCHEDULE.values(), default=0.0)
    if max_bonus > 0 and total_pnl > PROMOTION_PROFIT_MULTIPLIER * max_bonus:
        report.promotions.append(
            f"Total realised PnL = {total_pnl:.2f}, which exceeds "
            f"{PROMOTION_PROFIT_MULTIPLIER:.1f}× the largest bonus ({max_bonus:.2f}). "
            f"Check whether profit comes mainly from tier‑promotion rewards."
        )


def detect_time_decay(training: Dict, events: List[Dict], report: RedFlagReport):
    """
    Compute cumulative decay = steps × TIME_DECAY_PER_STEP.
    Compare it to net profit (if available) or to total PnL.
    Flag if profit is far below what is needed to offset decay.
    """
    steps = training.get("steps") or training.get("cumulative_steps")
    if not steps or not isinstance(steps, (int, float)):
        return

    cumulative_decay = TIME_DECAY_PER_STEP * steps  # negative value
    # Net profit: try to read explicit field first
    net_profit = training.get("net_profit")
    if net_profit is None:
        # Fallback to summed PnL from metric events
        net_profit = sum(
            ev.get("pnl", 0.0)
            for ev in events
            if ev.get("event") == "trade_closed"
        )

    # If profit is less than |decay| × TIME_DECAY_PROFIT_RATIO, flag it.
    required_profit = abs(cumulative_decay) * TIME_DECAY_PROFIT_RATIO
    if net_profit < required_profit:
        report.time_decay.append(
            f"Cumulative time‑decay over {int(steps)} steps = {cumulative_decay:.2f}. "
            f"Net profit ({net_profit:.2f}) is below the required "
            f"{required_profit:.2f} to offset decay, suggesting the model may be "
            f"forced into aggressive trading."
        )


def detect_data_ghosting(events: List[Dict], report: RedFlagReport):
    """
    Look for identical entry_price values appearing at multiple distinct
    timestamps (opened_at). Such duplication can indicate over‑fitting to a
    repeated data chunk.
    """
    price_to_timestamps: Dict[float, set] = defaultdict(set)

    for ev in events:
        if ev.get("event") != "trade_closed":
            continue
        entry_price = ev.get("entry_price")
        opened_at = ev.get("opened_at")
        if entry_price is None or opened_at is None:
            continue
        # Normalise price to two decimals to avoid floating‑point noise
        price_key = round(float(entry_price), 2)
        price_to_timestamps[price_key].add(opened_at)

    for price, timestamps in price_to_timestamps.items():
        if len(timestamps) > 1:
            report.ghost_entries.append(
                f"Entry price {price:.2f} appears in {len(timestamps)} distinct "
                f"timestamps. Possible data‑ghosting / over‑fitting."
            )


# --------------------------------------------------------------------------- #
# Main entry point
# --------------------------------------------------------------------------- #

def main() -> int:
    training_data = load_training_log()
    metric_events = load_metric_events()

    report = RedFlagReport()

    detect_promotion_bonus(training_data, metric_events, report)
    detect_time_decay(training_data, metric_events, report)
    detect_data_ghosting(metric_events, report)

    print(report.render())
    return 1 if report.any() else 0


if __name__ == "__main__":
    sys.exit(main())
