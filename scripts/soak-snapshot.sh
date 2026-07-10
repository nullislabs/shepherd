#!/bin/sh
# Hourly metrics collector for the 7-day grant soak.
# Runs inside the Alpine snapshotter container in docker-compose.soak.yml.
# Reaches the engine via Docker's internal DNS (http://engine:9100/metrics).
# BusyBox wget only - no package installs, so a registry/CDN blip at a
# container (re)start cannot crash-loop the snapshotter.
set -eu

# Scrape via a temp file so a failed scrape never leaves a truncated or
# empty evidence file; the caller decides whether failure is tolerable.
snap() {
    wget -q -O /tmp/snap "http://engine:9100/metrics" &&
        mv /tmp/snap "/evidence/$1-$(date -u +%Y%m%dT%H%M%SZ).txt"
}

# Baseline: retry until the first scrape lands. The engine is already
# healthy (depends_on: service_healthy), but a transient hiccup here
# must not exit the script - the restart policy would rerun it and each
# rerun writes a new metrics-start file, which the runbook reads as an
# operator restart.
until snap metrics-start; do
    sleep 10
done

while true; do
    sleep 3600
    snap metrics-snap || true
done
