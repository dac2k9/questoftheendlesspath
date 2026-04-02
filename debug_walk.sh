#!/usr/bin/env bash
# Simulate walking at a given speed (default 3.0 km/h)
# Usage: ./debug_walk.sh [speed] [player_id]
# Stop:  ./debug_walk.sh 0

SPEED="${1:-3.0}"
PLAYER="${2:-a0000000-0000-0000-0000-000000000001}"

echo "Debug walking at ${SPEED} km/h (player: ${PLAYER})"
echo "Press Ctrl+C to stop sending updates"

while true; do
  curl -sX POST http://localhost:3001/debug_walk \
    -H 'Content-Type: application/json' \
    -d "{\"player_id\":\"${PLAYER}\",\"speed\":${SPEED}}"
  sleep 1
done
