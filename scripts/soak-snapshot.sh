#!/bin/sh
# Hourly metrics collector for the 7-day grant soak.
# Runs inside the Alpine snapshotter container in docker-compose.soak.yml.
# Reaches the engine via Docker's internal DNS (http://engine:9100/metrics).
set -eu
apk add --no-cache curl -q >/dev/null 2>&1
curl -sf http://engine:9100/metrics > "/evidence/metrics-start-$(date -u +%Y%m%dT%H%M%SZ).txt"
while true; do
    sleep 3600
    curl -sf http://engine:9100/metrics > "/evidence/metrics-snap-$(date -u +%Y%m%dT%H%M%SZ).txt" || true
done
