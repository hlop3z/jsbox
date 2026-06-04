#!/usr/bin/env bash

DIR="$1"

SCRIPT_FILE="tests/$DIR/script.js"
CONTEXT_FILE="tests/$DIR/context.json"
CONFIG_FILE="tests/$DIR/secrets.json"

# Include the per-test config block only if the test dir provides one.
# Capabilities (mail, db, api) are opt-in: the global is injected only when
# its config is present, so a missing secrets.json => `mail`/`db` is undefined.
# Named secrets.json (not config.json) to avoid confusion with the service's
# config.json (engine/server limits) and because it holds SMTP/DB credentials.
if [ -f "$CONFIG_FILE" ]; then
  CONFIG_JSON="$(cat "$CONFIG_FILE")"
else
  CONFIG_JSON='{}'
fi

curl -X POST http://localhost:4172/execute \
  -H "Content-Type: application/json" \
  -d "$(jq -n \
    --arg script "$(cat "$SCRIPT_FILE")" \
    --argjson context "$(cat "$CONTEXT_FILE")" \
    --argjson config "$CONFIG_JSON" \
    '{script: $script, context: $context, config: $config}'
  )"
