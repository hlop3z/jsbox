#!/bin/sh
# End-to-end test for the `auth` capability against a real OIDC/IAM (Keycloak).
# Runs INSIDE a rust:alpine container that is on the same docker network as Keycloak
# (reachable as host `kc`). Builds jsbox, starts it, mints a real user token from
# Keycloak via the password grant, then drives /execute through the auth capability.
#
# Reproduce (from the repo root, PowerShell or sh):
#   docker network create jsbox-e2e
#   docker run -d --name kc --network jsbox-e2e \
#     -e KC_BOOTSTRAP_ADMIN_USERNAME=admin -e KC_BOOTSTRAP_ADMIN_PASSWORD=admin \
#     quay.io/keycloak/keycloak:26.0 start-dev
#   docker run --rm --network jsbox-e2e -v "$PWD:/work" -w /work \
#     rust:1.92-alpine sh /work/e2e_auth.sh
#   # teardown: docker rm -f kc && docker network rm jsbox-e2e
set -eu

KC="http://kc:8080"
ISSUER="$KC/realms/master"
JSBOX="http://127.0.0.1:3000"
PASS=0
FAIL=0

say() { printf '%s\n' "$*"; }
ok()  { PASS=$((PASS+1)); printf '  \033[32mPASS\033[0m %s\n' "$1"; }
bad() { FAIL=$((FAIL+1)); printf '  \033[31mFAIL\033[0m %s\n     %s\n' "$1" "$2"; }

say "==> Installing test tooling (curl, jq)"
apk add --no-cache curl jq >/dev/null 2>&1

say "==> Building jsbox (debug, musl)"
cargo build --quiet

say "==> Starting jsbox from /tmp (default config: 127.0.0.1:3000)"
( cd /tmp && /work/target/x86_64-unknown-linux-musl/debug/jsbox >/tmp/jsbox.log 2>&1 ) &
# Fall back to non-target-triple path if needed.
if [ ! -x /work/target/x86_64-unknown-linux-musl/debug/jsbox ]; then
  ( cd /tmp && /work/target/debug/jsbox >/tmp/jsbox.log 2>&1 ) &
fi

say "==> Waiting for jsbox /health"
for _ in $(seq 1 60); do
  if curl -fsS "$JSBOX/health" >/dev/null 2>&1; then break; fi
  sleep 1
done
curl -fsS "$JSBOX/health" >/dev/null 2>&1 || { say "jsbox did not come up"; cat /tmp/jsbox.log; exit 1; }
say "    jsbox up"

say "==> Waiting for Keycloak OIDC discovery"
for _ in $(seq 1 120); do
  if curl -fsS "$ISSUER/.well-known/openid-configuration" >/dev/null 2>&1; then break; fi
  sleep 2
done
curl -fsS "$ISSUER/.well-known/openid-configuration" >/dev/null 2>&1 || { say "Keycloak discovery never ready"; exit 1; }
say "    Keycloak up"

say "==> Minting a real user access token (admin-cli password grant)"
USER_TOKEN=$(curl -fsS -X POST "$ISSUER/protocol/openid-connect/token" \
  -d 'grant_type=password' -d 'client_id=admin-cli' -d 'scope=openid' \
  -d 'username=admin' -d 'password=admin' | jq -r .access_token)
[ -n "$USER_TOKEN" ] && [ "$USER_TOKEN" != "null" ] || { say "failed to mint token"; exit 1; }
say "    token minted (${#USER_TOKEN} chars)"

# Helper: POST a /execute body, echo the JSON response.
exec_jsbox() { curl -fsS -X POST "$JSBOX/execute" -H 'Content-Type: application/json' -d "$1"; }

say ""
say "  auth capability — end-to-end"
say ""

# --- 1. user_info with a valid token (exercises OIDC discovery + bearer userinfo) ---
BODY=$(jq -n --arg iss "$ISSUER" --arg tok "$USER_TOKEN" '{
  script: "function handler(ctx){ return json(auth.user_info(ctx.token), null); }",
  context: { token: $tok },
  config: { auth: { issuer: $iss } }
}')
RES=$(exec_jsbox "$BODY")
if [ "$(printf '%s' "$RES" | jq -r '.data.ok')" = "true" ] \
   && [ "$(printf '%s' "$RES" | jq -r '.data.claims.sub')" != "null" ]; then
  ok "user_info(valid) -> { ok:true, claims.sub present }"
else
  bad "user_info(valid)" "$RES"
fi

# meta.auth_requests should record the metered call (action user_info, status 200).
if [ "$(printf '%s' "$RES" | jq -r '.meta.auth_requests[0].action')" = "user_info" ] \
   && [ "$(printf '%s' "$RES" | jq -r '.meta.auth_requests[0].status')" = "200" ]; then
  ok "meta.auth_requests records the metered call (user_info, 200)"
else
  bad "meta.auth_requests" "$(printf '%s' "$RES" | jq -c '.meta.auth_requests')"
fi

# --- 2. user_info with a bad token -> in-band { ok:false, status:401 } (never throws) ---
BODY=$(jq -n --arg iss "$ISSUER" '{
  script: "function handler(ctx){ var u = auth.user_info(\"not-a-real-token\"); return json(u, null); }",
  config: { auth: { issuer: $iss } }
}')
RES=$(exec_jsbox "$BODY")
if [ "$(printf '%s' "$RES" | jq -r '.data.ok')" = "false" ] \
   && [ "$(printf '%s' "$RES" | jq -r '.data.status')" = "401" ] \
   && [ "$(printf '%s' "$RES" | jq -r '.data.code')" = "AUTH_INVALID_TOKEN" ] \
   && [ "$(printf '%s' "$RES" | jq -r '.error')" = "null" ]; then
  ok "user_info(bad) -> in-band { ok:false, status:401, AUTH_INVALID_TOKEN }, no throw"
else
  bad "user_info(bad)" "$RES"
fi

# --- 3. explicit userinfo_url override (no discovery) ---
BODY=$(jq -n --arg url "$ISSUER/protocol/openid-connect/userinfo" --arg tok "$USER_TOKEN" '{
  script: "function handler(ctx){ return json(auth.user_info(ctx.token).ok, null); }",
  context: { token: $tok },
  config: { auth: { issuer: "http://kc:8080/realms/master", userinfo_url: $url } }
}')
RES=$(exec_jsbox "$BODY")
if [ "$(printf '%s' "$RES" | jq -r '.data')" = "true" ]; then
  ok "user_info via explicit userinfo_url override"
else
  bad "user_info(userinfo_url override)" "$RES"
fi

# --- 4. introspect: create a confidential client, then RFC 7662 introspection ---
say "==> Creating a confidential client for introspection"
ADMIN_TOKEN="$USER_TOKEN"
curl -fsS -X POST "$KC/admin/realms/master/clients" \
  -H "Authorization: Bearer $ADMIN_TOKEN" -H 'Content-Type: application/json' \
  -d '{"clientId":"jsbox-introspect","publicClient":false,"serviceAccountsEnabled":true,"standardFlowEnabled":false,"directAccessGrantsEnabled":false}' \
  >/dev/null 2>&1 || true
CID=$(curl -fsS "$KC/admin/realms/master/clients?clientId=jsbox-introspect" \
  -H "Authorization: Bearer $ADMIN_TOKEN" | jq -r '.[0].id')
SECRET=$(curl -fsS "$KC/admin/realms/master/clients/$CID/client-secret" \
  -H "Authorization: Bearer $ADMIN_TOKEN" | jq -r '.value')
say "    client jsbox-introspect ($CID), secret ${#SECRET} chars"

BODY=$(jq -n --arg iss "$ISSUER" --arg tok "$USER_TOKEN" --arg cid "jsbox-introspect" --arg sec "$SECRET" '{
  script: "function handler(ctx){ return json(auth.introspect(ctx.token), null); }",
  context: { token: $tok },
  config: { auth: { issuer: $iss, client_id: $cid, client_secret: $sec } }
}')
RES=$(exec_jsbox "$BODY")
if [ "$(printf '%s' "$RES" | jq -r '.data.ok')" = "true" ] \
   && [ "$(printf '%s' "$RES" | jq -r '.data.claims.active')" = "true" ]; then
  ok "introspect(valid) -> { ok:true, claims.active:true }"
else
  bad "introspect(valid)" "$RES"
fi

# --- 5. introspect a bogus token -> active:false (200, ok:true) ---
BODY=$(jq -n --arg iss "$ISSUER" --arg cid "jsbox-introspect" --arg sec "$SECRET" '{
  script: "function handler(ctx){ return json(auth.introspect(\"bogus\").claims.active, null); }",
  config: { auth: { issuer: $iss, client_id: $cid, client_secret: $sec } }
}')
RES=$(exec_jsbox "$BODY")
if [ "$(printf '%s' "$RES" | jq -r '.data')" = "false" ]; then
  ok "introspect(bogus) -> claims.active:false"
else
  bad "introspect(bogus)" "$RES"
fi

# --- 6. introspect without client creds -> tagged capability error (throws) ---
BODY=$(jq -n --arg iss "$ISSUER" '{
  script: "function handler(ctx){ try { auth.introspect(\"x\"); return json(\"no-throw\", null); } catch(e){ return json(\"threw\", null); } }",
  config: { auth: { issuer: $iss } }
}')
RES=$(exec_jsbox "$BODY")
if [ "$(printf '%s' "$RES" | jq -r '.data')" = "threw" ]; then
  ok "introspect without client creds throws (caught in JS)"
else
  bad "introspect(no creds)" "$RES"
fi

# --- 7. per-request caching: two user_info calls, one metered op ---
BODY=$(jq -n --arg iss "$ISSUER" --arg tok "$USER_TOKEN" '{
  script: "function handler(ctx){ var a=auth.user_info(ctx.token); var b=auth.user_info(ctx.token); return json(a.claims.sub===b.claims.sub, null); }",
  context: { token: $tok },
  config: { auth: { issuer: $iss } }
}')
RES=$(exec_jsbox "$BODY")
N=$(printf '%s' "$RES" | jq -r '.meta.auth_requests | length')
if [ "$(printf '%s' "$RES" | jq -r '.data')" = "true" ] && [ "$N" = "1" ]; then
  ok "two user_info(token) calls -> cached (1 metered op)"
else
  bad "user_info caching" "data=$(printf '%s' "$RES" | jq -c '.data') auth_requests=$N"
fi

say ""
say "------------------------------------"
if [ "$FAIL" -eq 0 ]; then
  printf '  \033[32mOK\033[0m %d/%d auth e2e checks passed\n\n' "$PASS" "$((PASS+FAIL))"
  exit 0
else
  printf '  \033[31mFAIL\033[0m %d passed, %d failed\n\n' "$PASS" "$FAIL"
  exit 1
fi
