#!/usr/bin/env python3
"""Integration tests for jsbox."""

import json
import os
import subprocess
import sys
import time
import urllib.error
import urllib.parse
import urllib.request

BASE_URL = "http://127.0.0.1:3000"

# -- Identity providers for the `auth` capability (override via env for CI/in-network) --
# Defaults match `docker compose up` on the host (Keycloak :8081, ZITADEL :8082).
KEYCLOAK_ISSUER = os.environ.get("KEYCLOAK_ISSUER", "http://localhost:8081/realms/master")
KEYCLOAK_ADMIN_USER = os.environ.get("KEYCLOAK_ADMIN", "admin")
KEYCLOAK_ADMIN_PASS = os.environ.get("KEYCLOAK_ADMIN_PASSWORD", "admin")
ZITADEL_ISSUER = os.environ.get("ZITADEL_ISSUER", "http://localhost:8082")
# ZITADEL needs a service-account PAT. Provide it directly (ZITADEL_PAT) or via a file
# (ZITADEL_PAT_FILE). Extract it after `docker compose up`:
#   docker compose exec zitadel cat /tmp/zitadel-admin-sa.pat
ZITADEL_PAT = os.environ.get("ZITADEL_PAT", "")
ZITADEL_PAT_FILE = os.environ.get("ZITADEL_PAT_FILE", "")


# -- Test runner -------------------------------------------------------------

class Runner:
    """Minimal test runner with pass/fail tracking."""

    def __init__(self):
        self.passed = 0
        self.failed = 0

    @property
    def total(self):
        return self.passed + self.failed

    def section(self, title: str):
        print(f"\n\033[1m  {title}\033[0m\n")

    def test(self, name: str, body: dict, check):
        resp = _post(body)
        try:
            assert resp is not None, "no response"
            assert check(resp), "assertion failed"
            self.passed += 1
            print(f"  \033[32mPASS\033[0m {name}")
        except Exception as exc:
            self.failed += 1
            print(f"  \033[31mFAIL\033[0m {name}")
            print(f"       {exc}")
            if resp:
                print(f"       {json.dumps(resp)}")

    def summary(self):
        print("\n" + "-" * 36)
        if self.failed == 0:
            print(f"  \033[32mOK\033[0m {self.passed}/{self.total} tests passed")
        else:
            print(f"  \033[31mFAIL\033[0m {self.passed} passed, {self.failed} failed out of {self.total}")
        print()


# -- HTTP helpers ------------------------------------------------------------

def _post(body: dict) -> dict | None:
    data = json.dumps(body).encode()
    req = urllib.request.Request(
        f"{BASE_URL}/execute",
        data=data,
        headers={"Content-Type": "application/json"},
    )
    try:
        with urllib.request.urlopen(req, timeout=30) as resp:
            return json.loads(resp.read())
    except urllib.error.HTTPError as err:
        return json.loads(err.read())
    except Exception:
        return None


# -- Script helpers ----------------------------------------------------------

def h(body: str, ctx=None, config=None) -> dict:
    """Build a request body from a handler function body."""
    req = {"script": f"function handler(ctx) {{ {body} }}"}
    if ctx is not None:
        req["context"] = ctx
    if config is not None:
        req["config"] = config
    return req


def h_raw(script: str, ctx=None, config=None) -> dict:
    """Build a request body from raw script source."""
    req = {"script": script}
    if ctx is not None:
        req["context"] = ctx
    if config is not None:
        req["config"] = config
    return req


# -- Assertion helpers -------------------------------------------------------

def data_eq(expected):
    """Assert data == expected and no error."""
    return lambda r: r["data"] == expected and r["error"] is None


def data_is_none():
    return lambda r: r["data"] is None


def has_error():
    return lambda r: r["error"] is not None


def error_contains(text: str):
    return lambda r: r["error"] is not None and text in str(r["error"])


def data_none_with_error():
    return lambda r: r["data"] is None and r["error"] is not None


# -- Test definitions --------------------------------------------------------

def test_functionality(t: Runner):
    t.section("Functionality")
    t.test("sum of two numbers",       h("return json(ctx.a + ctx.b, null);", {"a": 10, "b": 20}), data_eq(30))
    t.test("string result",            h('return json("hello " + ctx.name, null);', {"name": "Alice"}), data_eq("hello Alice"))
    t.test("object with map",          h("return json({items: ctx.list.map(function(x){return x*2}), count: ctx.list.length}, null);", {"list": [1, 2, 3]}),
           lambda r: r["data"]["items"] == [2, 4, 6] and r["data"]["count"] == 3)
    t.test("array result",             h("return json([1,2,3], null);"), data_eq([1, 2, 3]))
    t.test("null when no return",      h("json(null, null);"), data_is_none())
    t.test("boolean result",           h("return json(ctx.x > 5, null);", {"x": 10}), data_eq(True))
    t.test("nested context",           h("return json(ctx.user.name, null);", {"user": {"name": "Bob"}}), data_eq("Bob"))
    t.test("default empty context",    h("return json(Object.keys(ctx).length, null);"), data_eq(0))


def test_user_errors(t: Runner):
    t.section("User-defined errors")
    t.test("push error messages",
           h('var e = {messages: []}; if (!ctx.name) e.messages.push("name required"); return json(null, e);'),
           lambda r: r["error"]["messages"][0] == "name required")
    t.test("custom error object",
           h('return json(null, {code: 400, detail: "bad input"});'),
           lambda r: r["error"]["code"] == 400 and r["error"]["detail"] == "bad input")
    t.test("data with warnings",
           h('return json({status: "ok"}, {warnings: ["low battery"]});'),
           lambda r: r["data"]["status"] == "ok" and r["error"]["warnings"][0] == "low battery")


def test_exceptions(t: Runner):
    t.section("Exception handling")
    t.test("throw returns error",      h('throw new Error("boom");'),       data_none_with_error())
    t.test("missing handler",          h_raw("var x = 1;"),                 has_error())
    t.test("syntax error",             h_raw("function handler(ctx { }"),   has_error())


def test_sandbox(t: Runner):
    t.section("Sandbox hardening")
    t.test("infinite loop times out",  h("while(true){}"),                              error_contains("timed out"))
    t.test("memory bomb stopped",      h("var a=[]; while(true) a.push(new Array(100000));"), has_error())
    t.test("eval() blocked",           h('return json(eval("1+1"), null);'),            data_none_with_error())
    t.test("deep recursion stopped",   h("function f(n){return f(n+1)} return json(f(0), null);"), data_none_with_error())


def test_json_bridge(t: Runner):
    t.section("json() bridge")
    t.test("data only",               h("return json(42);"),               data_eq(42))
    t.test("null data and error",      h("return json(null, null);"),       lambda r: r["data"] is None and r["error"] is None)


def test_meta(t: Runner):
    t.section("Meta")
    simple = h("return json(1, null);")
    t.test("has script_bytes",         simple, lambda r: r["meta"]["script_bytes"] > 0)
    t.test("has context_bytes",        h("return json(1, null);", {"a": 1}), lambda r: r["meta"]["context_bytes"] > 0)
    t.test("total = script + context", simple, lambda r: r["meta"]["total_input_bytes"] == r["meta"]["script_bytes"] + r["meta"]["context_bytes"])
    t.test("has exec_time_us",         simple, lambda r: r["meta"]["exec_time_us"] >= 0)


def test_http_api(t: Runner):
    t.section("HTTP api")
    wildcard = {"allowed_hosts": ["*"]}
    httpbin  = {"allowed_hosts": ["httpbin.org"]}
    blocked  = {"allowed_hosts": ["example.com"]}

    t.test("disabled when no config",
           h("return json(typeof api, null);"),
           data_eq("undefined"))
    t.test("available with wildcard",
           h("return json(typeof api, null);", config=wildcard),
           data_eq("object"))
    t.test("get with wildcard",
           h("var r = api.get('https://httpbin.org/get', {foo:'bar'}); return json({status:r.status, ok:r.data!==null}, null);", config=wildcard),
           lambda r: r["data"]["status"] == 200 and r["data"]["ok"] is True)
    t.test("get with specific host",
           h("var r = api.get('https://httpbin.org/get'); return json(r.status, null);", config=httpbin),
           data_eq(200))
    t.test("get blocked by host",
           h("var r = api.get('https://httpbin.org/get'); return json(r, null);", config=blocked),
           lambda r: r["data"]["status"] == 0)
    t.test("post with body",
           h('var r = api.post("https://httpbin.org/post", {hello:"world"}); return json(r.status, null);', config=httpbin),
           data_eq(200))
    t.test("delete works",
           h("var r = api.delete('https://httpbin.org/delete'); return json(r.status, null);", config=httpbin),
           data_eq(200))

    # Headers
    t.test("get with auth header",
           h("var r = api.get('https://httpbin.org/get', null, {'Authorization': 'Bearer test123'}); return json(r.data.headers.Authorization, null);", config=httpbin),
           data_eq("Bearer test123"))
    t.test("post with custom header",
           h('var r = api.post("https://httpbin.org/post", {a:1}, {"X-Custom": "foo"}); return json(r.data.headers["X-Custom"], null);', config=httpbin),
           data_eq("foo"))
    t.test("content-type cannot be overridden",
           h('var r = api.post("https://httpbin.org/post", {a:1}, {"Content-Type": "text/plain"}); return json(r.data.headers["Content-Type"], null);', config=httpbin),
           data_eq("application/json"))
    t.test("delete with header",
           h("var r = api.delete('https://httpbin.org/delete', {'X-Req-Id': '42'}); return json(r.data.headers['X-Req-Id'], null);", config=httpbin),
           data_eq("42"))


# -- Database tests ----------------------------------------------------------

PG_CONFIG = {"host": "localhost", "port": 5432, "user": "test", "password": "test", "database": "testdb"}
CR_CONFIG = {"host": "localhost", "port": 26257, "user": "root", "password": "", "database": "testdb"}

SETUP_SQL = """
    DROP TABLE IF EXISTS test_types;
    DROP TABLE IF EXISTS test_txn;
    CREATE TABLE IF NOT EXISTS test_types (
        id SERIAL PRIMARY KEY,
        big BIGINT,
        num NUMERIC(10,2),
        flag BOOLEAN,
        name TEXT,
        data JSONB,
        ts TIMESTAMPTZ DEFAULT NOW(),
        uid UUID DEFAULT gen_random_uuid()
    );
    INSERT INTO test_types (big, num, flag, name, data)
    VALUES (9223372036854775807, 12345.67, true, 'Alice', '{"key":"val"}');
    CREATE TABLE IF NOT EXISTS test_txn (id SERIAL PRIMARY KEY, val TEXT);
"""


def _db_available(config: dict) -> bool:
    """Check if a database is reachable."""
    resp = _post(h("db.query('SELECT 1 as ok'); return json('up', null);", config={"db": config}))
    return resp is not None and resp.get("data") == "up"


def _setup_db(config: dict):
    """Create test tables."""
    for stmt in SETUP_SQL.strip().split(";"):
        stmt = stmt.strip()
        if stmt:
            _post(h(f"db.execute(\"{stmt}\"); return json('ok', null);", config={{"db": config}}))


def test_db_engine(t: Runner, label: str, db: dict):
    """Run DB tests against a specific engine (Postgres or CockroachDB)."""
    t.section(f"Database ({label})")

    # Setup tables
    setup_script = SETUP_SQL.replace("'", "\\'").replace("\n", " ")
    for stmt in [s.strip() for s in SETUP_SQL.strip().split(";") if s.strip()]:
        safe = stmt.replace("'", "\\'").replace("\n", " ")
        _post(h(f"db.execute('{safe}'); return json('ok', null);", config={"db": db}))

    # Basic query (CockroachDB returns INT8 for literals, so "1" as string)
    is_crdb = label == "CockroachDB"
    t.test(f"{label}: SELECT 1",
           h("var r = db.query('SELECT 1 as num'); return json(r.rows[0].num, null);", config={"db": db}),
           data_eq("1") if is_crdb else data_eq(1))

    # Column metadata
    t.test(f"{label}: columns returned",
           h("var r = db.query('SELECT 1 as a, 2 as b'); return json(r.columns, null);", config={"db": db}),
           data_eq(["a", "b"]))

    # Row count
    t.test(f"{label}: row_count",
           h("var r = db.query('SELECT 1 UNION ALL SELECT 2'); return json(r.row_count, null);", config={"db": db}),
           data_eq(2))

    # Parameterized query
    t.test(f"{label}: params",
           h("var r = db.query('SELECT $1::text as name', ['Bob']); return json(r.rows[0].name, null);", config={"db": db}),
           data_eq("Bob"))

    # Boolean param
    t.test(f"{label}: bool param",
           h("var r = db.query('SELECT $1::boolean as flag', [true]); return json(r.rows[0].flag, null);", config={"db": db}),
           data_eq(True))

    # BIGINT always string
    t.test(f"{label}: bigint is string",
           h("var r = db.query('SELECT big FROM test_types'); return json(typeof r.rows[0].big, null);", config={"db": db}),
           data_eq("string"))

    t.test(f"{label}: bigint value",
           h("var r = db.query('SELECT big FROM test_types'); return json(r.rows[0].big, null);", config={"db": db}),
           data_eq("9223372036854775807"))

    # NUMERIC as string
    t.test(f"{label}: numeric is string",
           h("var r = db.query('SELECT num FROM test_types'); return json(typeof r.rows[0].num, null);", config={"db": db}),
           data_eq("string"))

    # INT4 as number (CockroachDB SERIAL is INT8 → string)
    t.test(f"{label}: int4 is number",
           h("var r = db.query('SELECT id FROM test_types'); return json(typeof r.rows[0].id, null);", config={"db": db}),
           data_eq("string") if is_crdb else data_eq("number"))

    # Boolean column
    t.test(f"{label}: bool column",
           h("var r = db.query('SELECT flag FROM test_types'); return json(r.rows[0].flag, null);", config={"db": db}),
           data_eq(True))

    # TEXT column
    t.test(f"{label}: text column",
           h("var r = db.query('SELECT name FROM test_types'); return json(r.rows[0].name, null);", config={"db": db}),
           data_eq("Alice"))

    # JSONB pass-through
    t.test(f"{label}: jsonb pass-through",
           h("var r = db.query('SELECT data FROM test_types'); return json(r.rows[0].data.key, null);", config={"db": db}),
           data_eq("val"))

    # UUID is string
    t.test(f"{label}: uuid is string",
           h("var r = db.query('SELECT uid FROM test_types'); return json(typeof r.rows[0].uid, null);", config={"db": db}),
           data_eq("string"))

    # TIMESTAMP is string
    t.test(f"{label}: timestamp is string",
           h("var r = db.query('SELECT ts FROM test_types'); return json(typeof r.rows[0].ts, null);", config={"db": db}),
           data_eq("string"))

    # NULL handling
    t.test(f"{label}: null value",
           h("var r = db.query('SELECT NULL as x'); return json(r.rows[0].x, null);", config={"db": db}),
           lambda r: r["data"] is None)

    # Execute (INSERT)
    t.test(f"{label}: execute insert",
           h("var r = db.execute(\"INSERT INTO test_txn (val) VALUES ('exec_test')\"); return json(r.rows_affected, null);", config={"db": db}),
           data_eq(1))

    # Execute (UPDATE)
    t.test(f"{label}: execute update",
           h("var r = db.execute(\"UPDATE test_txn SET val = 'updated' WHERE val = 'exec_test'\"); return json(r.rows_affected, null);", config={"db": db}),
           data_eq(1))

    # Transactions: commit
    t.test(f"{label}: begin + commit",
           h("db.begin(); db.execute(\"INSERT INTO test_txn (val) VALUES ('txn_commit')\"); db.commit(); var r = db.query(\"SELECT val FROM test_txn WHERE val = 'txn_commit'\"); return json(r.row_count, null);", config={"db": db}),
           data_eq(1))

    # Transactions: rollback
    t.test(f"{label}: begin + rollback",
           h("db.begin(); db.execute(\"INSERT INTO test_txn (val) VALUES ('txn_rollback')\"); db.rollback(); var r = db.query(\"SELECT val FROM test_txn WHERE val = 'txn_rollback'\"); return json(r.row_count, null);", config={"db": db}),
           data_eq(0))

    # Auto-rollback on throw
    t.test(f"{label}: auto-rollback on error",
           h("db.begin(); db.execute(\"INSERT INTO test_txn (val) VALUES ('txn_auto')\"); throw new Error('oops');", config={"db": db}),
           has_error())

    # max_rows truncation
    t.test(f"{label}: max_rows truncation",
           h("var r = db.query('SELECT generate_series(1, 50)'); return json(r.truncated, null);", config={"db": {**db, "max_rows": 5}}),
           data_eq(True))

    # max_rows row_count
    t.test(f"{label}: max_rows caps count",
           h("var r = db.query('SELECT generate_series(1, 50)'); return json(r.row_count, null);", config={"db": {**db, "max_rows": 5}}),
           data_eq(5))

    # SQL error
    t.test(f"{label}: sql error throws",
           h("db.query('SELECT * FROM nonexistent_table_xyz'); return json('should not reach', null);", config={"db": db}),
           has_error())

    # db disabled without config
    t.test(f"{label}: db disabled without config",
           h("return json(typeof db, null);"),
           data_eq("undefined"))

    # Bad connection
    t.test(f"{label}: bad connection",
           h("db.query('SELECT 1');", config={"db": {**db, "host": "nonexistent.invalid", "port": 1}}),
           has_error())

    # Metrics tracked
    t.test(f"{label}: metrics tracked",
           h("db.query('SELECT 1'); db.query('SELECT 2'); return json(1, null);", config={"db": db}),
           lambda r: len(r["meta"]["db_requests"]) == 2)

    # Cleanup
    _post(h("db.execute('DROP TABLE IF EXISTS test_types'); db.execute('DROP TABLE IF EXISTS test_txn'); return json('ok', null);", config={"db": db}))


# -- Auth (OIDC/IAM) tests ---------------------------------------------------

def _provider_req(url: str, method: str = "GET", form=None, payload=None, headers=None):
    """Talk directly to an identity provider. Returns (status, parsed_json|None)."""
    data = None
    hdrs = dict(headers or {})
    if form is not None:
        data = urllib.parse.urlencode(form).encode()
        hdrs.setdefault("Content-Type", "application/x-www-form-urlencoded")
    elif payload is not None:
        data = json.dumps(payload).encode()
        hdrs.setdefault("Content-Type", "application/json")
    req = urllib.request.Request(url, data=data, headers=hdrs, method=method)
    try:
        with urllib.request.urlopen(req, timeout=20) as resp:
            raw = resp.read()
            return resp.status, (json.loads(raw) if raw else None)
    except urllib.error.HTTPError as err:
        try:
            return err.code, json.loads(err.read())
        except Exception:
            return err.code, None
    except Exception:
        return None, None


def _discovery_ok(issuer: str) -> bool:
    """True if the issuer publishes a usable OIDC discovery document."""
    status, body = _provider_req(f"{issuer}/.well-known/openid-configuration")
    return status == 200 and bool(body) and "userinfo_endpoint" in body


def _keycloak_token() -> str | None:
    """Mint a real user access token via the admin-cli password grant (openid scope)."""
    status, body = _provider_req(
        f"{KEYCLOAK_ISSUER}/protocol/openid-connect/token",
        method="POST",
        form={
            "grant_type": "password",
            "client_id": "admin-cli",
            "scope": "openid",
            "username": KEYCLOAK_ADMIN_USER,
            "password": KEYCLOAK_ADMIN_PASS,
        },
    )
    return body.get("access_token") if status == 200 and body else None


def _keycloak_introspect_creds(admin_token: str) -> dict | None:
    """Ensure a confidential client exists and return its client_id/secret for RFC 7662."""
    base, _, realm = KEYCLOAK_ISSUER.rpartition("/realms/")
    auth = {"Authorization": f"Bearer {admin_token}"}
    cid = "jsbox-introspect"
    # Create it (ignore an "already exists" 409 from a previous run).
    _provider_req(
        f"{base}/admin/realms/{realm}/clients",
        method="POST",
        payload={
            "clientId": cid,
            "publicClient": False,
            "serviceAccountsEnabled": True,
            "standardFlowEnabled": False,
            "directAccessGrantsEnabled": False,
        },
        headers=auth,
    )
    status, arr = _provider_req(f"{base}/admin/realms/{realm}/clients?clientId={cid}", headers=auth)
    if status != 200 or not arr:
        return None
    internal_id = arr[0]["id"]
    status, sec = _provider_req(
        f"{base}/admin/realms/{realm}/clients/{internal_id}/client-secret", headers=auth
    )
    if status != 200 or not sec or not sec.get("value"):
        return None
    return {"client_id": cid, "client_secret": sec["value"]}


def _zitadel_token() -> str | None:
    """Read the ZITADEL service-account PAT from env or a file."""
    if ZITADEL_PAT.strip():
        return ZITADEL_PAT.strip()
    if ZITADEL_PAT_FILE and os.path.exists(ZITADEL_PAT_FILE):
        with open(ZITADEL_PAT_FILE, encoding="utf-8") as handle:
            return handle.read().strip()
    return None


def test_auth_provider(t: Runner, label: str, issuer: str, token: str, introspect: dict | None):
    """Drive the `auth` capability against a real OIDC/IAM (provider-agnostic)."""
    t.section(f"Auth ({label})")
    cfg = {"auth": {"issuer": issuer}}
    ctx = {"token": token}

    t.test(f"{label}: disabled without config",
           h("return json(typeof auth, null);"),
           data_eq("undefined"))

    # Valid token: OIDC discovery + bearer userinfo → claims.
    t.test(f"{label}: user_info(valid) -> ok:true",
           h("return json(auth.user_info(ctx.token).ok, null);", ctx, cfg),
           data_eq(True))
    t.test(f"{label}: user_info resolves claims.sub",
           h("var u = auth.user_info(ctx.token); return json(u.ok && typeof u.claims.sub === 'string', null);", ctx, cfg),
           data_eq(True))

    # Bad token is the caller's business flow → in-band, never thrown.
    t.test(f"{label}: user_info(bad) -> in-band, no throw",
           h("return json(auth.user_info('garbage-token-value'), null);", config=cfg),
           lambda r: r["data"]["ok"] is False
                     and r["data"]["code"] == "AUTH_INVALID_TOKEN"
                     and r["error"] is None)

    # Metered + per-request cache (two calls for one token = one round trip).
    t.test(f"{label}: metered in auth_requests",
           h("auth.user_info(ctx.token); return json(1, null);", ctx, cfg),
           lambda r: len(r["meta"]["auth_requests"]) == 1
                     and r["meta"]["auth_requests"][0]["action"] == "user_info")
    t.test(f"{label}: per-token cache (2 calls, 1 op)",
           h("auth.user_info(ctx.token); auth.user_info(ctx.token); return json(1, null);", ctx, cfg),
           lambda r: len(r["meta"]["auth_requests"]) == 1)

    # Infra/misconfig throws a tagged capability error (here: introspect w/o creds).
    t.test(f"{label}: introspect without creds throws",
           h("try { auth.introspect('x'); return json('no-throw', null); } catch (e) { return json('threw', null); }", config=cfg),
           data_eq("threw"))

    if introspect:
        icfg = {"auth": {"issuer": issuer, "client_id": introspect["client_id"], "client_secret": introspect["client_secret"]}}
        t.test(f"{label}: introspect(valid) -> active:true",
               h("return json(auth.introspect(ctx.token).claims.active, null);", ctx, icfg),
               data_eq(True))
        t.test(f"{label}: introspect(bogus) -> active:false",
               h("return json(auth.introspect('bogus').claims.active, null);", config=icfg),
               data_eq(False))


def run_auth_tests(t: Runner):
    """Run the auth suite against whichever providers are reachable."""
    # Keycloak — mint a token + a confidential client live.
    if _discovery_ok(KEYCLOAK_ISSUER):
        kc_token = _keycloak_token()
        if kc_token:
            test_auth_provider(t, "Keycloak", KEYCLOAK_ISSUER, kc_token, _keycloak_introspect_creds(kc_token))
        else:
            print("\n  \033[33mSKIP\033[0m Keycloak auth tests (reachable but token mint failed)\n")
    else:
        print("\n  \033[33mSKIP\033[0m Keycloak auth tests (not running — use: docker compose up -d keycloak)\n")

    # ZITADEL — needs a service-account PAT (introspection needs an API app, so it is
    # exercised on Keycloak; ZITADEL covers discovery + userinfo + the throw path).
    zt_token = _zitadel_token()
    if zt_token and _discovery_ok(ZITADEL_ISSUER):
        test_auth_provider(t, "Zitadel", ZITADEL_ISSUER, zt_token, None)
    elif zt_token:
        print("\n  \033[33mSKIP\033[0m Zitadel auth tests (PAT set but issuer unreachable)\n")
    else:
        print("\n  \033[33mSKIP\033[0m Zitadel auth tests (no ZITADEL_PAT — see docker-compose.yml)\n")


# -- Main --------------------------------------------------------------------

def _wait_for_server() -> bool:
    for _ in range(20):
        if _post(h("return json(1, null);")) is not None:
            return True
        time.sleep(0.5)
    return False


def main():
    server_proc = None

    if not _wait_for_server():
        print("Starting server...")
        server_proc = subprocess.Popen(["cargo", "run"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        time.sleep(4)
        if not _wait_for_server():
            print("ERROR: Server failed to start")
            sys.exit(1)

    print(f"\n\033[1mRunning tests against {BASE_URL}\033[0m")

    t = Runner()
    test_functionality(t)
    test_user_errors(t)
    test_exceptions(t)
    test_sandbox(t)
    test_json_bridge(t)
    test_meta(t)
    test_http_api(t)

    # Database tests — only if containers are running
    if _db_available(PG_CONFIG):
        test_db_engine(t, "PostgreSQL", PG_CONFIG)
    else:
        print("\n  \033[33mSKIP\033[0m PostgreSQL tests (not running — use: docker compose up -d)\n")

    if _db_available(CR_CONFIG):
        test_db_engine(t, "CockroachDB", CR_CONFIG)
    else:
        print("\n  \033[33mSKIP\033[0m CockroachDB tests (not running — use: docker compose up -d)\n")

    # Auth tests — only against identity providers that are reachable
    run_auth_tests(t)

    t.summary()

    if server_proc:
        server_proc.terminate()

    sys.exit(t.failed)


if __name__ == "__main__":
    main()
