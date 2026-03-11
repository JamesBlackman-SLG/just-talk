#!/bin/bash
set -e

pkill -x just-talk 2>/dev/null && sleep 0.3 || true

cargo build --release

LOG=/tmp/just-talk.log
> "$LOG"

nohup ./target/release/just-talk >> "$LOG" 2>&1 &
disown

echo "just-talk running (pid $!) — logs: $LOG"
