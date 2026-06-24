#!/bin/sh
# Per-capability cfg sweep for runlet-core (the deterministic-core feature matrix).
# Runs the real gate (plain `cargo clippy`, NOT --all-targets) for each single capability
# plus the no-capability build. fabric-backends is featureless and not swept here.
set -e
for f in NONE db mongo mail redis amq auth http s3; do
  if [ "$f" = "NONE" ]; then
    echo "=== runlet-core --no-default-features ==="
    cargo clippy -p runlet-core --no-default-features --quiet 2>&1 | tail -n 30
  else
    echo "=== runlet-core --no-default-features --features $f ==="
    cargo clippy -p runlet-core --no-default-features --features "$f" --quiet 2>&1 | tail -n 30
  fi
done
echo "=== sweep done ==="
