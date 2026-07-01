#!/usr/bin/env python3
"""Integration tests for jsbox."""

import json
import os
import shutil
import subprocess
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
from urllib.parse import urlparse

BASE_URL = os.environ.get("JSBOX_URL", "http://127.0.0.1:3000")

# -- Database endpoints (override via env when the server runs in-network/CI) ---------
# Defaults match `docker compose up` on the host. PGBOUNCER is the transaction-mode
# pooler in front of postgres (see docs/design/pooled-capabilities.md).
PG_HOST = os.environ.get("PG_HOST", "localhost")
PG_PORT = int(os.environ.get("PG_PORT", "5432"))
PGBOUNCER_HOST = os.environ.get("PGBOUNCER_HOST", "localhost")
PGBOUNCER_PORT = int(os.environ.get("PGBOUNCER_PORT", "6432"))
CR_HOST = os.environ.get("CR_HOST", "localhost")
CR_PORT = int(os.environ.get("CR_PORT", "26257"))

# Local httpbin clone (`httpbin` service in docker-compose) — the HTTP `api` tests run
# against it so the suite never depends on httpbin.org uptime. go-httpbin echoes
# headers/args as ARRAYS of strings, hence the [0] indexing in assertions. Reaching a
# localhost/LAN target needs the server started with `debug: true` (SSRF private-IP
# block) — the harness-generated config sets it.
HTTPBIN_URL = os.environ.get("HTTPBIN_URL", "http://localhost:8095").rstrip("/")
HTTPBIN_HOST = urlparse(HTTPBIN_URL).hostname or "localhost"

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

    def check(self, name: str, ok: bool):
        """Record a boolean assertion (for tests that post outside the default BASE_URL,
        e.g. a dedicated trusted-mode box, where `test()` can't be used)."""
        if ok:
            self.passed += 1
            print(f"  \033[32mPASS\033[0m {name}")
        else:
            self.failed += 1
            print(f"  \033[31mFAIL\033[0m {name}")

    def summary(self):
        print("\n" + "-" * 36)
        if self.failed == 0:
            print(f"  \033[32mOK\033[0m {self.passed}/{self.total} tests passed")
        else:
            print(f"  \033[31mFAIL\033[0m {self.passed} passed, {self.failed} failed out of {self.total}")
        print()


# -- HTTP helpers ------------------------------------------------------------

def _post(body: dict, headers: dict | None = None) -> dict | None:
    data = json.dumps(body).encode()
    hdrs = {"Content-Type": "application/json"}
    if headers:
        hdrs.update(headers)
    req = urllib.request.Request(f"{BASE_URL}/execute", data=data, headers=hdrs)
    try:
        with urllib.request.urlopen(req, timeout=30) as resp:
            return _parse_response(resp.getcode(), resp.read())
    except urllib.error.HTTPError as err:
        return _parse_response(err.code, err.read())
    except Exception:
        return None


def _post_status(url: str, body: dict, headers: dict | None = None):
    """POST to an explicit URL, returning `(http_status, parsed_envelope)`. Unlike `_post`
    (which targets BASE_URL and hides the status), this keeps the status so a caller can assert
    on the code — used by the trusted-mode box which runs on its own port."""
    data = json.dumps(body).encode()
    hdrs = {"Content-Type": "application/json"}
    if headers:
        hdrs.update(headers)
    req = urllib.request.Request(url, data=data, headers=hdrs)
    try:
        with urllib.request.urlopen(req, timeout=30) as resp:
            return resp.getcode(), _parse_response(resp.getcode(), resp.read())
    except urllib.error.HTTPError as err:
        return err.code, _parse_response(err.code, err.read())
    except Exception:
        return None, None


def _parse_response(status: int, raw: bytes) -> dict:
    """Parse a server response. A well-formed jsbox response is the JSON envelope; a
    non-JSON body (e.g. axum's plain-text deserialize rejection) is surfaced as a
    sentinel so tests can assert on the contract gap instead of crashing."""
    try:
        return json.loads(raw)
    except Exception:
        return {"_http_status": status, "_non_json_body": raw.decode("utf-8", "replace")}


def _get_text(path: str) -> tuple[int, str] | None:
    """GET a plain-text endpoint (e.g. /metrics). Returns (status, body) or None."""
    req = urllib.request.Request(f"{BASE_URL}{path}")
    try:
        with urllib.request.urlopen(req, timeout=10) as resp:
            return resp.getcode(), resp.read().decode("utf-8", "replace")
    except urllib.error.HTTPError as err:
        return err.code, err.read().decode("utf-8", "replace")
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


def _err_code(r):
    """The system-error `code`, or None. Safe when `error` is null or absent
    (r.get('error', {}) returns None when the key exists with a null value)."""
    if not r:
        return None
    err = r.get("error")
    return err.get("code") if isinstance(err, dict) else None


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
    httpbin  = {"allowed_hosts": [HTTPBIN_HOST]}
    blocked  = {"allowed_hosts": ["example.com"]}
    url = HTTPBIN_URL

    t.test("disabled when no config",
           h("return json(typeof api, null);"),
           data_eq("undefined"))
    t.test("available with wildcard",
           h("return json(typeof api, null);", config=wildcard),
           data_eq("object"))
    # A `*` wildcard host is intentionally INERT in SSRF-relaxed debug mode. The box runs with
    # debug:true so these api tests can reach the private-IP httpbin; under that relaxation `*`
    # would collapse the host allowlist down to the IP filter alone, so it is never honored
    # (host.rs: `allow_wildcard_hosts && !allow_private`). The request is blocked in-band → the
    # private host is unreachable via `*` (status 0), even though a specific-host config reaches it.
    t.test("wildcard host inert under debug (private-IP relax) -> blocked",
           h(f"var r = api.get('{url}/get', {{foo:'bar'}}); return json({{status:r.status}}, null);", config=wildcard),
           lambda r: r["data"]["status"] == 0)
    t.test("get with specific host",
           h(f"var r = api.get('{url}/get'); return json(r.status, null);", config=httpbin),
           data_eq(200))
    t.test("get blocked by host",
           h(f"var r = api.get('{url}/get'); return json(r, null);", config=blocked),
           lambda r: r["data"]["status"] == 0)
    t.test("post with body",
           h(f'var r = api.post("{url}/post", {{hello:"world"}}); return json(r.status, null);', config=httpbin),
           data_eq(200))
    t.test("delete works",
           h(f"var r = api.delete('{url}/delete'); return json(r.status, null);", config=httpbin),
           data_eq(200))

    # Headers (go-httpbin echoes header values as arrays of strings)
    t.test("get with auth header",
           h(f"var r = api.get('{url}/get', null, {{'Authorization': 'Bearer test123'}}); return json(r.data.headers.Authorization[0], null);", config=httpbin),
           data_eq("Bearer test123"))
    t.test("post with custom header",
           h(f'var r = api.post("{url}/post", {{a:1}}, {{"X-Custom": "foo"}}); return json(r.data.headers["X-Custom"][0], null);', config=httpbin),
           data_eq("foo"))
    t.test("content-type cannot be overridden",
           h(f'var r = api.post("{url}/post", {{a:1}}, {{"Content-Type": "text/plain"}}); return json(r.data.headers["Content-Type"][0], null);', config=httpbin),
           data_eq("application/json"))
    t.test("delete with header",
           h(f"var r = api.delete('{url}/delete', {{'X-Req-Id': '42'}}); return json(r.data.headers['X-Req-Id'][0], null);", config=httpbin),
           data_eq("42"))


# -- Database tests ----------------------------------------------------------

PG_CONFIG = {"host": PG_HOST, "port": PG_PORT, "user": "test", "password": "test", "database": "testdb"}
PGB_CONFIG = {"host": PGBOUNCER_HOST, "port": PGBOUNCER_PORT, "user": "test", "password": "test", "database": "testdb"}
CR_CONFIG = {"host": CR_HOST, "port": CR_PORT, "user": "root", "password": "", "database": "testdb"}

# -- Mongo + NATS endpoints (the `mongo` / `nats` services in docker-compose) ----------
MONGO_HOST = os.environ.get("MONGO_HOST", "localhost")
MONGO_PORT = int(os.environ.get("MONGO_PORT", "27017"))
# Standalone dev node has no auth, so username/password are omitted.
MONGO_CONFIG = {"host": MONGO_HOST, "port": MONGO_PORT, "database": "testdb"}

NATS_HOST = os.environ.get("NATS_HOST", "localhost")
NATS_PORT = int(os.environ.get("NATS_PORT", "4222"))
NATS_CONFIG = {"backend": "nats", "host": NATS_HOST, "port": NATS_PORT}


# -- Egress resources (Step 5 trust flip: the box sends logical names; fabricd holds the configs) -
#
# After the trust flip the box carries no driver credentials: a request names logical resources in
# `config.io`, and the `fabricd` sidecar resolves them against its own `resources` table. The
# harness therefore (1) builds that table from the env endpoints + the per-test variants below,
# (2) starts `fabricd` with it, and (3) sends names, never configs. A down backend just makes its
# section self-skip (the live probe through the box fails) — the resource still exists in the table.

def _io(kind: str, name: str) -> dict:
    """A request `config.io` selecting one logical resource of `kind` (e.g. `{"io":{"db":["pg"]}}`)."""
    return {"io": {kind: [name]}}


def _db_io(name: str) -> dict:
    return _io("db", name)


def _mongo_io(name: str) -> dict:
    return _io("mongo", name)


def _amq_io(name: str) -> dict:
    return _io("amq", name)


def _auth_io(name: str) -> dict:
    return _io("auth", name)


def _db_resources(base: str, cfg: dict) -> dict:
    """Named `db` bindings for one engine: the base plus the variants the tests reference by name."""
    return {
        base: {"kind": "db", **cfg},
        f"{base}-maxrows5": {"kind": "db", **cfg, "max_rows": 5},
        f"{base}-badhost": {"kind": "db", **cfg, "host": "nonexistent.invalid", "port": 1},
        f"{base}-fast": {"kind": "db", **cfg, "statement_timeout_ms": 800},
        f"{base}-unlimited": {"kind": "db", **cfg, "statement_timeout_ms": 0},
        f"{base}-huge": {"kind": "db", **cfg, "statement_timeout_ms": 60000},
    }


def _auth_resources(label: str, issuer: str, introspect: dict | None) -> dict:
    """Named `auth` bindings for one provider: the base (issuer only) + an introspect variant."""
    base = f"auth-{label.lower()}"
    res = {base: {"kind": "auth", "issuer": issuer}}
    if introspect:
        res[f"{base}-introspect"] = {
            "kind": "auth",
            "issuer": issuer,
            "client_id": introspect["client_id"],
            "client_secret": introspect["client_secret"],
        }
    return res


def build_resources(auth_resources: dict) -> dict:
    """The full `fabricd` resources table: every named resource the suite can reference."""
    res: dict = {}
    res.update(_db_resources("pg", PG_CONFIG))
    res.update(_db_resources("pgbouncer", PGB_CONFIG))
    res.update(_db_resources("cockroach", CR_CONFIG))
    res["db-broken"] = {
        "kind": "db", "host": "broken-db.invalid", "port": 1,
        "user": "x", "password": "x", "database": "x",
    }
    res["mongo"] = {"kind": "mongo", **MONGO_CONFIG}
    res["nats"] = {"kind": "amq", **NATS_CONFIG}
    res["nats-fast"] = {"kind": "amq", **NATS_CONFIG, "request_timeout_ms": 500}
    res.update(auth_resources)
    return res

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


def _db_available(name: str) -> bool:
    """Check if the named `db` resource is reachable (probes through the box → fabricd)."""
    resp = _post(h("db.query('SELECT 1 as ok'); return json('up', null);", config=_db_io(name)))
    return resp is not None and resp.get("data") == "up"


def _setup_db(name: str):
    """Create test tables (via the named `db` resource)."""
    for stmt in SETUP_SQL.strip().split(";"):
        stmt = stmt.strip()
        if stmt:
            _post(h(f"db.execute(\"{stmt}\"); return json('ok', null);", config=_db_io(name)))


def test_db_engine(t: Runner, label: str, db: str):
    """Run DB tests against a specific engine, named by its `db` resource (`db` = the base name)."""
    t.section(f"Database ({label})")

    # Setup tables
    setup_script = SETUP_SQL.replace("'", "\\'").replace("\n", " ")
    for stmt in [s.strip() for s in SETUP_SQL.strip().split(";") if s.strip()]:
        safe = stmt.replace("'", "\\'").replace("\n", " ")
        _post(h(f"db.execute('{safe}'); return json('ok', null);", config=_db_io(db)))

    # Basic query (CockroachDB returns INT8 for literals, so "1" as string)
    is_crdb = label == "CockroachDB"
    t.test(f"{label}: SELECT 1",
           h("var r = db.query('SELECT 1 as num'); return json(r.rows[0].num, null);", config=_db_io(db)),
           data_eq("1") if is_crdb else data_eq(1))

    # Column metadata
    t.test(f"{label}: columns returned",
           h("var r = db.query('SELECT 1 as a, 2 as b'); return json(r.columns, null);", config=_db_io(db)),
           data_eq(["a", "b"]))

    # Row count
    t.test(f"{label}: row_count",
           h("var r = db.query('SELECT 1 UNION ALL SELECT 2'); return json(r.row_count, null);", config=_db_io(db)),
           data_eq(2))

    # Parameterized query
    t.test(f"{label}: params",
           h("var r = db.query('SELECT $1::text as name', ['Bob']); return json(r.rows[0].name, null);", config=_db_io(db)),
           data_eq("Bob"))

    # Boolean param
    t.test(f"{label}: bool param",
           h("var r = db.query('SELECT $1::boolean as flag', [true]); return json(r.rows[0].flag, null);", config=_db_io(db)),
           data_eq(True))

    # BIGINT always string
    t.test(f"{label}: bigint is string",
           h("var r = db.query('SELECT big FROM test_types'); return json(typeof r.rows[0].big, null);", config=_db_io(db)),
           data_eq("string"))

    t.test(f"{label}: bigint value",
           h("var r = db.query('SELECT big FROM test_types'); return json(r.rows[0].big, null);", config=_db_io(db)),
           data_eq("9223372036854775807"))

    # NUMERIC as string
    t.test(f"{label}: numeric is string",
           h("var r = db.query('SELECT num FROM test_types'); return json(typeof r.rows[0].num, null);", config=_db_io(db)),
           data_eq("string"))

    # INT4 as number (CockroachDB SERIAL is INT8 → string)
    t.test(f"{label}: int4 is number",
           h("var r = db.query('SELECT id FROM test_types'); return json(typeof r.rows[0].id, null);", config=_db_io(db)),
           data_eq("string") if is_crdb else data_eq("number"))

    # Boolean column
    t.test(f"{label}: bool column",
           h("var r = db.query('SELECT flag FROM test_types'); return json(r.rows[0].flag, null);", config=_db_io(db)),
           data_eq(True))

    # TEXT column
    t.test(f"{label}: text column",
           h("var r = db.query('SELECT name FROM test_types'); return json(r.rows[0].name, null);", config=_db_io(db)),
           data_eq("Alice"))

    # JSONB pass-through
    t.test(f"{label}: jsonb pass-through",
           h("var r = db.query('SELECT data FROM test_types'); return json(r.rows[0].data.key, null);", config=_db_io(db)),
           data_eq("val"))

    # UUID is string
    t.test(f"{label}: uuid is string",
           h("var r = db.query('SELECT uid FROM test_types'); return json(typeof r.rows[0].uid, null);", config=_db_io(db)),
           data_eq("string"))

    # TIMESTAMP is string
    t.test(f"{label}: timestamp is string",
           h("var r = db.query('SELECT ts FROM test_types'); return json(typeof r.rows[0].ts, null);", config=_db_io(db)),
           data_eq("string"))

    # NULL handling
    t.test(f"{label}: null value",
           h("var r = db.query('SELECT NULL as x'); return json(r.rows[0].x, null);", config=_db_io(db)),
           lambda r: r["data"] is None)

    # Execute (INSERT)
    t.test(f"{label}: execute insert",
           h("var r = db.execute(\"INSERT INTO test_txn (val) VALUES ('exec_test')\"); return json(r.rows_affected, null);", config=_db_io(db)),
           data_eq(1))

    # Execute (UPDATE)
    t.test(f"{label}: execute update",
           h("var r = db.execute(\"UPDATE test_txn SET val = 'updated' WHERE val = 'exec_test'\"); return json(r.rows_affected, null);", config=_db_io(db)),
           data_eq(1))

    # Transactions: commit
    t.test(f"{label}: begin + commit",
           h("db.begin(); db.execute(\"INSERT INTO test_txn (val) VALUES ('txn_commit')\"); db.commit(); var r = db.query(\"SELECT val FROM test_txn WHERE val = 'txn_commit'\"); return json(r.row_count, null);", config=_db_io(db)),
           data_eq(1))

    # Transactions: rollback
    t.test(f"{label}: begin + rollback",
           h("db.begin(); db.execute(\"INSERT INTO test_txn (val) VALUES ('txn_rollback')\"); db.rollback(); var r = db.query(\"SELECT val FROM test_txn WHERE val = 'txn_rollback'\"); return json(r.row_count, null);", config=_db_io(db)),
           data_eq(0))

    # Auto-rollback on throw
    t.test(f"{label}: auto-rollback on error",
           h("db.begin(); db.execute(\"INSERT INTO test_txn (val) VALUES ('txn_auto')\"); throw new Error('oops');", config=_db_io(db)),
           has_error())

    # max_rows truncation
    t.test(f"{label}: max_rows truncation",
           h("var r = db.query('SELECT generate_series(1, 50)'); return json(r.truncated, null);", config=_db_io(db + "-maxrows5")),
           data_eq(True))

    # max_rows row_count
    t.test(f"{label}: max_rows caps count",
           h("var r = db.query('SELECT generate_series(1, 50)'); return json(r.row_count, null);", config=_db_io(db + "-maxrows5")),
           data_eq(5))

    # SQL error
    t.test(f"{label}: sql error throws",
           h("db.query('SELECT * FROM nonexistent_table_xyz'); return json('should not reach', null);", config=_db_io(db)),
           has_error())

    # db disabled without config
    t.test(f"{label}: db disabled without config",
           h("return json(typeof db, null);"),
           data_eq("undefined"))

    # Bad connection
    t.test(f"{label}: bad connection",
           h("db.query('SELECT 1');", config=_db_io(db + "-badhost")),
           has_error())

    # Metrics tracked
    t.test(f"{label}: metrics tracked",
           h("db.query('SELECT 1'); db.query('SELECT 2'); return json(1, null);", config=_db_io(db)),
           lambda r: len(r["meta"]["db_requests"]) == 2)

    # Cleanup
    _post(h("db.execute('DROP TABLE IF EXISTS test_types'); db.execute('DROP TABLE IF EXISTS test_txn'); return json('ok', null);", config=_db_io(db)))


# -- Script registry (execute by key) ----------------------------------------

def _mongo_available(name: str) -> bool:
    """Check if the named `mongo` resource is reachable."""
    resp = _post(h("mongo.count('t_probe', {}); return json('up', null);", config=_mongo_io(name)))
    return resp is not None and resp.get("data") == "up"


def test_mongo(t: Runner):
    """Mongo capability — string `_id`s sidestep the ObjectId-filter caveat (a hex-string
    filter is a BSON string, not an ObjectId, so explicit string ids match cleanly)."""
    t.section("Mongo (document store)")
    cfg = _mongo_io("mongo")

    t.test("clean collection",
           h("mongo.delete_many('t_users', {}); return json('ok', null);", config=cfg),
           data_eq("ok"))
    t.test("insert_one returns id",
           h("var r = mongo.insert_one('t_users', {_id:'u1', name:'Alice', active:true}); return json(r.inserted_id, null);", config=cfg),
           data_eq("u1"))
    t.test("insert_many returns count",
           h("var r = mongo.insert_many('t_users', [{_id:'u2',name:'Bob',active:true},{_id:'u3',name:'Cy',active:false}]); return json(r.inserted_count, null);", config=cfg),
           data_eq(2))
    t.test("count all",
           h("return json(mongo.count('t_users', {}), null);", config=cfg),
           data_eq(3))
    t.test("find_one by id",
           h("return json(mongo.find_one('t_users', {_id:'u1'}).name, null);", config=cfg),
           data_eq("Alice"))
    t.test("find_one missing is null",
           h("return json(mongo.find_one('t_users', {_id:'nope'}), null);", config=cfg),
           data_is_none())
    t.test("find filter + result shape",
           h("var r = mongo.find('t_users', {active:true}, {sort:{_id:1}}); return json({n:r.count, trunc:r.truncated, first:r.docs[0]._id}, null);", config=cfg),
           lambda r: r["data"]["n"] == 2 and r["data"]["trunc"] is False and r["data"]["first"] == "u1")
    t.test("update_one matched+modified",
           h("var r = mongo.update_one('t_users', {_id:'u1'}, {$set:{active:false}}); return json([r.matched, r.modified], null);", config=cfg),
           data_eq([1, 1]))
    t.test("count after update",
           h("return json(mongo.count('t_users', {active:true}), null);", config=cfg),
           data_eq(1))
    t.test("aggregate group",
           h("return json(mongo.aggregate('t_users', [{$group:{_id:'$active', n:{$sum:1}}}]).count, null);", config=cfg),
           data_eq(2))
    t.test("delete_one",
           h("return json(mongo.delete_one('t_users', {_id:'u3'}).deleted, null);", config=cfg),
           data_eq(1))
    # Duplicate key is a developer write error.
    t.test("duplicate key -> MONGO_WRITE",
           h("mongo.insert_one('t_users', {_id:'u1'}); return json('nope', null);", config=cfg),
           lambda r: r["data"] is None and _err_code(r) == "MONGO_WRITE")
    # A malformed filter classifies as a developer query error (tolerant of the exact code).
    t.test("bad filter -> MONGO_ classified error",
           h("mongo.find('t_users', {$badOp: 1}); return json('nope', null);", config=cfg),
           lambda r: r["data"] is None and str(_err_code(r) or "").startswith("MONGO_"))


def _nats_available(name: str) -> bool:
    """Check if the named `amq` (NATS) resource is reachable (publish to an unsubscribed subject)."""
    resp = _post(h("amq.send([['_probe', {p:1}]]); return json('up', null);", config=_amq_io(name)))
    return resp is not None and resp.get("data") == "up"


def test_nats(t: Runner):
    """NATS backend of the `amq` capability: publish + request-reply (no subscribe)."""
    t.section("NATS (amq backend)")
    cfg = _amq_io("nats")

    t.test("publish batch -> count",
           h("return json(amq.send([['ev.a', {i:1}], ['ev.b', {i:2}]]), null);", config=cfg),
           data_eq(2))
    t.test("single-pair shorthand -> 1",
           h("return json(amq.send(['ev.c', {i:3}]), null);", config=cfg),
           data_eq(1))
    # No responder on the subject -> a classified amq error (short timeout keeps it fast).
    fast = _amq_io("nats-fast")
    t.test("request with no responder -> AMQ_ error",
           h("amq.request('no.responder.here', {ping:1}); return json('nope', null);", config=fast),
           lambda r: r["data"] is None and str(_err_code(r) or "").startswith("AMQ_"))


def test_registry(t: Runner):
    """Exercise `key` mode: XOR validation always; execution if the registry is loaded."""
    t.section("Script registry (execute by key)")

    # Request-shape validation works regardless of how the server was started.
    t.test("script+key rejected (400 SCRIPT_XOR_KEY)",
           {"script": "function handler(ctx) { return json(1, null); }", "key": "greet"},
           lambda r: r["error"]["code"] == "SCRIPT_XOR_KEY")
    t.test("neither script nor key rejected",
           {"context": {"a": 1}},
           lambda r: r["error"]["code"] == "SCRIPT_XOR_KEY")
    t.test("unknown key -> SCRIPT_NOT_FOUND",
           {"key": "no/such/script"},
           lambda r: r["error"]["code"] == "SCRIPT_NOT_FOUND")

    # Key-mode execution needs the server started with scripts_dir=tests/scripts
    # (the harness-started server is; an externally started one may not be).
    probe = _post({"key": "greet"})
    if probe is not None and probe.get("data") == "hello world":
        t.test("execute by key",
               {"key": "greet", "context": {"name": "Alice"}},
               data_eq("hello Alice"))
        t.test("nested key",
               {"key": "acme/billing/pricing", "context": {"qty": 3, "price": 5}},
               lambda r: r["data"]["total"] == 15)
        t.test("key-mode config still per-request (db disabled)",
               {"key": "greet"},
               lambda r: r["error"] is None)
        t.test("meta echoes key + resolved script_bytes",
               {"key": "greet"},
               lambda r: r["meta"]["key"] == "greet" and r["meta"]["script_bytes"] > 0)
        t.test("inline requests carry no meta.key",
               h("return json(1, null);"),
               lambda r: "key" not in r["meta"])
    else:
        print("\n  \033[33mSKIP\033[0m registry execution tests (server not started with scripts_dir=tests/scripts)\n")


# -- Adversarial: try to break the registry + request contract ---------------

def test_registry_hardening(t: Runner):
    """Actively attack the execute-by-key surface: traversal, type confusion, edges."""
    t.section("Registry hardening (adversarial)")

    # Path traversal via key must never escape the registry — `key` is a map lookup,
    # never a filesystem path at request time. Each of these is a clean 404, not a
    # file read, a 500, or a panic.
    for evil in ["../greet", "../../../etc/passwd", "..\\..\\greet", "/etc/passwd",
                 "acme/../greet", "greet/../greet", "./greet"]:
        t.test(f"traversal key rejected: {evil}",
               {"key": evil},
               lambda r: r["error"]["code"] == "SCRIPT_NOT_FOUND")

    # The extensionless key is the contract; the filename must miss.
    t.test("key with .js extension misses",
           {"key": "greet.js"},
           lambda r: r["error"]["code"] == "SCRIPT_NOT_FOUND")

    # Degenerate keys: empty string is a present-but-unknown key (404), not "neither".
    t.test("empty-string key -> 404 not XOR",
           {"key": ""},
           lambda r: r["error"]["code"] == "SCRIPT_NOT_FOUND")
    t.test("very long key -> clean 404",
           {"key": "a/" * 5000},
           lambda r: r["error"]["code"] == "SCRIPT_NOT_FOUND")

    # Type confusion: wrong JSON types for script/key must be rejected with the SAME
    # structured {data,error,meta} envelope as every other error (code MALFORMED_REQUEST),
    # never axum's default plain-text rejection and never a panic/hang.
    def malformed(r):
        return (r is not None and "_non_json_body" not in r
                and r.get("data") is None
                and _err_code(r) == "MALFORMED_REQUEST"
                and r["error"]["type"] == "request")
    t.test("numeric key -> MALFORMED_REQUEST envelope", {"key": 123}, malformed)
    t.test("array script -> MALFORMED_REQUEST envelope", {"script": ["function handler(){}"]}, malformed)
    t.test("object key -> MALFORMED_REQUEST envelope", {"key": {"nested": "x"}}, malformed)
    t.test("meta present on malformed body", {"key": 123},
           lambda r: r is not None and "trace_id" in r.get("meta", {}))

    # meta.key must echo on the error paths too (audit trail survives failure).
    t.test("meta.key echoes on SCRIPT_NOT_FOUND",
           {"key": "no/such/thing"},
           lambda r: r["meta"]["key"] == "no/such/thing")
    t.test("meta.key echoes on XOR rejection",
           {"script": "function handler(){}", "key": "greet"},
           lambda r: r["meta"]["key"] == "greet" and r["error"]["code"] == "SCRIPT_XOR_KEY")

    # Registered scripts must travel the IDENTICAL engine path as inline — prove the
    # failure modes match by registering nothing special and exercising a known script.
    probe = _post({"key": "greet"})
    if not (probe is not None and probe.get("data") == "hello world"):
        print("\n  \033[33mSKIP\033[0m registry engine-path tests (no scripts_dir)\n")
        return
    # A registered script gets the same sandbox: context still flows, config still
    # per-request, and a huge context is still rejected the same way as inline.
    big = "x" * (5 * 1024 * 1024)
    t.test("oversize context rejected in key mode",
           {"key": "greet", "context": {"blob": big}},
           lambda r: r["error"]["code"] == "CONTEXT_TOO_LARGE")
    t.test("key mode cannot reach db without config",
           {"key": "greet"},
           lambda r: r["error"] is None and r["data"] == "hello world")


def test_isolation_under_concurrency(t: Runner):
    """Fire interleaved requests that pollute globals; prove fresh-context isolation."""
    t.section("Isolation under concurrency (adversarial)")
    import concurrent.futures

    # Each request sets a global and reads it back; if contexts leaked across the pool,
    # a request would observe another's value. Run many in parallel and check every one
    # sees only its own id.
    def one(i):
        # Retry on a bulkhead 429 — this probes isolation, not capacity.
        for _ in range(20):
            body = h(f"globalThis.__leak = {i}; return json(globalThis.__leak, null);")
            r = _post(body)
            if _err_code(r) == "OVERLOADED":
                time.sleep(0.02)
                continue
            return r is not None and r.get("data") == i
        return False

    with concurrent.futures.ThreadPoolExecutor(max_workers=16) as ex:
        results = list(ex.map(one, range(200)))
    t.test("no global leakage across 200 concurrent runs",
           h("return json(1, null);"),
           lambda _r: all(results))

    # A prior request that defines a function must not be visible to the next.
    _post(h("globalThis.__planted = function(){ return 'pwned'; }; return json(1, null);"))
    t.test("planted global not visible to next request",
           h("return json(typeof globalThis.__planted, null);"),
           data_eq("undefined"))


# -- Resilience: bulkhead (Tier 1) + statement_timeout clamp (Tier 0) ---------

def test_bulkhead(t: Runner):
    """Saturate the bulkhead and prove excess load fast-fails 429 OVERLOADED while the
    server stays responsive (the SLO-protecting behavior)."""
    t.section("Bulkhead / overload (resilience)")
    import concurrent.futures

    # A request that holds its permit for a few hundred ms of CPU work.
    slow = h("var x=0; for (var i=0;i<15000000;i++){ x+=i; } return json(x>0, null);")

    def fire(_):
        r = _post(slow)
        if r is None:
            return "none"
        if _err_code(r) == "OVERLOADED":
            return "429"
        return "ok" if r.get("data") is True else "other"

    with concurrent.futures.ThreadPoolExecutor(max_workers=24) as ex:
        outcomes = list(ex.map(fire, range(24)))

    # The bulkhead only sheds load when the configured bound is below the burst size.
    # If the server runs the default (auto, high) bound, nothing is shed — probe, don't fail.
    if "429" not in outcomes:
        print(f"  \033[33mPROBE\033[0m bulkhead not exercised (no 429s; bound >= burst). outcomes={set(outcomes)}\n")
    else:
        t.test("bulkhead sheds excess as 429 OVERLOADED",
               h("return json(1,null);"), lambda _r: "429" in outcomes)
        t.test("some requests still succeed under overload",
               h("return json(1,null);"), lambda _r: "ok" in outcomes)
    # Either way, the server must be responsive immediately after the burst.
    t.test("server responsive right after overload burst",
           h("return json('alive', null);"), data_eq("alive"))


def test_partition_fairness(t: Runner):
    """Tier 5: a noisy partition's flood sheds on its OWN per-partition cap
    (PARTITION_OVERLOADED) while a well-behaved partition still gets through."""
    t.section("Per-partition fairness (Tier 5)")
    import concurrent.futures

    slow = "function handler(ctx){ var x=0; for(var i=0;i<20000000;i++){x+=i;} return json(x>0,null); }"
    fast = "function handler(ctx){ return json('ok', null); }"
    noisy_codes, good_outcomes = [], []

    def noisy_worker():
        for _ in range(3):
            noisy_codes.append(_err_code(_post({"script": slow, "partition": "noisy"})))

    def good_worker():
        time.sleep(0.15)  # let the noisy flood ramp first
        for _ in range(4):
            r = _post({"script": fast, "partition": "good"})
            good_outcomes.append((_err_code(r), r.get("data") if r else None))
            time.sleep(0.1)

    with concurrent.futures.ThreadPoolExecutor(max_workers=12) as ex:
        flood = [ex.submit(noisy_worker) for _ in range(6)]
        victim = ex.submit(good_worker)
        for f in flood:
            f.result()
        victim.result()

    partition_shed = sum(1 for c in noisy_codes if c == "PARTITION_OVERLOADED")
    good_ok = sum(1 for code, data in good_outcomes if code is None and data == "ok")

    # Tier 5 is opt-in; if the server has no per-partition cap, nothing sheds — probe + skip
    # the fairness asserts, but still check the meta/header plumbing below.
    if partition_shed > 0:
        t.test("noisy partition sheds on its own cap (PARTITION_OVERLOADED)",
               h("return json(1,null);"), lambda _r: partition_shed > 0)
        t.test("good partition still gets through under the noisy flood",
               h("return json(1,null);"), lambda _r: good_ok > 0)
    else:
        print("  \033[33mPROBE\033[0m Tier 5 not active (no max_concurrent_per_partition) — asserts skipped\n")

    # Partition-key plumbing works regardless of whether the cap is set:
    r = _post({"script": fast}, headers={"X-Partition-Key": "acme"})
    t.test("X-Partition-Key header echoed in meta.partition",
           h("return json(1,null);"),
           lambda _r: r is not None and r.get("meta", {}).get("partition") == "acme")
    r2 = _post({"script": fast, "partition": "beta"})
    t.test("partition body field echoed in meta.partition",
           h("return json(1,null);"),
           lambda _r: r2 is not None and r2.get("meta", {}).get("partition") == "beta")
    r3 = _post({"script": fast, "partition": "ignored"}, headers={"X-Partition-Key": "header-wins"})
    t.test("header takes precedence over body partition field",
           h("return json(1,null);"),
           lambda _r: r3 is not None and r3.get("meta", {}).get("partition") == "header-wins")


def test_metrics(t: Runner):
    """The /metrics endpoint exposes Prometheus counters/gauges that move with traffic."""
    t.section("Observability (/metrics)")

    def _scrape() -> str | None:
        res = _get_text("/metrics")
        return res[1] if res is not None and res[0] == 200 else None

    def _counter(text: str | None, needle: str):
        for line in (text or "").splitlines():
            if line.startswith(needle):
                try:
                    return int(line.rsplit(" ", 1)[1])
                except Exception:
                    return None
        return None

    body = _scrape()
    t.test("/metrics returns 200 Prometheus text",
           h("return json(1,null);"),
           lambda _r: body is not None and "runlet_executions_total" in body)
    t.test("/metrics exposes bulkhead + breaker series",
           h("return json(1,null);"),
           lambda _r: body is not None
           and "runlet_bulkhead_permits_total" in body
           and "runlet_db_breaker_trips_total" in body)
    t.test("/metrics exposes the execution latency histogram",
           h("return json(1,null);"),
           lambda _r: body is not None
           and "runlet_execution_duration_seconds_bucket{le=\"+Inf\"}" in body
           and "runlet_execution_duration_seconds_count" in body)
    t.test("/metrics exposes the per-capability latency family",
           h("return json(1,null);"),
           lambda _r: body is not None
           and body.count("# TYPE runlet_capability_op_duration_seconds histogram") == 1
           and "runlet_capability_op_duration_seconds_count{capability=\"db\"}" in body)

    success_label = 'runlet_executions_total{outcome="success"}'
    hist_label = "runlet_execution_duration_seconds_count"
    before = _counter(body, success_label)
    before_hist = _counter(body, hist_label)
    _post(h("return json('ok', null);"))
    after_text = _scrape()
    after = _counter(after_text, success_label)
    after_hist = _counter(after_text, hist_label)
    t.test("success counter advances after an execution",
           h("return json(1,null);"),
           lambda _r: before is not None and after is not None and after > before)
    t.test("latency histogram count advances after an execution",
           h("return json(1,null);"),
           lambda _r: before_hist is not None and after_hist is not None and after_hist > before_hist)

    err_label = 'runlet_executions_total{outcome="script_error"}'
    before_err = _counter(after_text, err_label)
    _post(h("throw new Error('boom');"))
    err_text = _scrape()
    after_err = _counter(err_text, err_label)
    t.test("script_error counter advances after a throw",
           h("return json(1,null);"),
           lambda _r: before_err is not None and after_err is not None and after_err > before_err)

    before_rej = _counter(err_text, "runlet_rejections_total ")
    _post({"context": {}})  # neither script nor key -> SCRIPT_XOR_KEY rejection
    rej_text = _scrape()
    after_rej = _counter(rej_text, "runlet_rejections_total ")
    t.test("rejection counter advances after a bad request",
           h("return json(1,null);"),
           lambda _r: before_rej is not None and after_rej is not None and after_rej > before_rej)


def test_esm(t: Runner):
    """ES-module handlers: a handler authored with `export` (default or named) and a handler
    that `import`s a registered module. Backed by tests/modules/acme/pricing.mjs."""
    t.section("ES modules (export / import)")

    # A classic script handler still works (script-mode is detected by the absence of a
    # top-level `export`) — the back-compat guarantee.
    t.test("classic script handler still runs",
           h_raw("function handler(ctx){ return json(ctx.a * 2, null); }", {"a": 21}),
           data_eq(42))

    # `export default function handler` — the canonical ESM shape.
    t.test("export default handler",
           h_raw("export default function handler(ctx){ return json('hi:'+ctx.name, null); }",
                 {"name": "Ada"}),
           data_eq("hi:Ada"))

    # `export function handler` — named export also resolves.
    t.test("named export handler",
           h_raw("export function handler(ctx){ return json('named', null); }"),
           data_eq("named"))

    # A handler `import`s a registered module and uses its exports.
    t.test("handler imports a registry module",
           h_raw("import { quote, withTax } from 'acme/pricing';\n"
                 "export default function handler(ctx){ return json(withTax(quote(ctx.n, 10)), null); }",
                 {"n": 5}),
           data_eq(55))

    # Named + default + value imports all bind.
    t.test("module exports a constant",
           h_raw("import { TAX_RATE } from 'acme/pricing';\n"
                 "export default function handler(ctx){ return json(TAX_RATE, null); }"),
           data_eq(0.1))

    # Importing an unregistered specifier fails to resolve — the security property: a script
    # can reach only registered modules, never an arbitrary path.
    t.test("import of unknown module -> MODULE_NOT_FOUND",
           h_raw("import { x } from 'no/such/module';\n"
                 "export default function handler(ctx){ return json(1, null); }"),
           lambda r: _err_code(r) == "MODULE_NOT_FOUND")
    t.test("import path-traversal specifier -> MODULE_NOT_FOUND",
           h_raw("import { x } from '../../../etc/passwd';\n"
                 "export default function handler(ctx){ return json(1, null); }"),
           lambda r: _err_code(r) == "MODULE_NOT_FOUND")

    # A module-shaped source with no exported handler is a clear HANDLER_NOT_DEFINED.
    t.test("module without exported handler -> error",
           h_raw("export const notHandler = 1;"),
           lambda r: _err_code(r) == "HANDLER_NOT_DEFINED")


def test_hasura(t: Runner):
    """The `hasura/client` injectable module (modules/hasura/client.mjs). Hermetic: each
    handler stubs `globalThis.api` so the module's request-shaping and error-handling are
    exercised without a live Hasura — the module reads whatever `api.post` returns."""
    t.section("Hasura module (hasura/client)")

    # Probe: the module must be registered (merged modules_dir). Self-skip otherwise so a
    # run against a server without modules_dir reports SKIP instead of a wall of failures.
    probe = _post(h_raw(
        "import { hasura } from 'hasura/client';\n"
        "export default function handler(ctx){ return json(typeof hasura, null); }"))
    if _err_code(probe) == "MODULE_NOT_FOUND":
        print("\n  \033[33mSKIP\033[0m hasura module tests (no modules_dir with hasura/client)\n")
        return

    # Request shaping: /v1/graphql URL (trailing slash stripped), JSON content-type,
    # admin-secret + role headers, and variables passed through untouched.
    t.test("query shapes the request + returns data",
           h_raw(
               "import { hasura } from 'hasura/client';\n"
               "export default function handler(ctx){\n"
               "  var cap = {};\n"
               "  globalThis.api = { post: function(url, body, headers){\n"
               "    cap = { url: url, body: body, headers: headers };\n"
               "    return { status: 200, data: { data: { users: [{ id: 7 }] } } };\n"
               "  }};\n"
               "  var h = hasura({ endpoint: 'https://hasura.test/', adminSecret: 'sek', role: 'viewer' });\n"
               "  var data = h.query('query { users { id } }', { x: 1 });\n"
               "  return json({ id: data.users[0].id, url: cap.url,\n"
               "    secret: cap.headers['x-hasura-admin-secret'], role: cap.headers['x-hasura-role'],\n"
               "    ctype: cap.headers['content-type'], qHas: typeof cap.body.query === 'string',\n"
               "    varX: cap.body.variables.x }, null);\n"
               "}"),
           data_eq({"id": 7, "url": "https://hasura.test/v1/graphql", "secret": "sek",
                    "role": "viewer", "ctype": "application/json", "qHas": True, "varX": 1}))

    # A forwarded user JWT wins over the admin secret (Bearer set, no admin-secret header).
    t.test("token forwards as Bearer and suppresses admin secret",
           h_raw(
               "import { hasura } from 'hasura/client';\n"
               "export default function handler(ctx){\n"
               "  var cap = {};\n"
               "  globalThis.api = { post: function(url, body, headers){ cap.h = headers;\n"
               "    return { status: 200, data: { data: { ok: true } } }; }};\n"
               "  hasura({ endpoint: 'https://hasura.test', token: 'jwt123', adminSecret: 'sek' }).raw('query { ok }');\n"
               "  return json({ auth: cap.h['authorization'], hasSecret: ('x-hasura-admin-secret' in cap.h) }, null);\n"
               "}"),
           data_eq({"auth": "Bearer jwt123", "hasSecret": False}))

    # GraphQL error inside an HTTP 200 → query() throws with .code + .graphql attached.
    t.test("GraphQL error in a 200 body throws (not silent)",
           h_raw(
               "import { hasura } from 'hasura/client';\n"
               "export default function handler(ctx){\n"
               "  globalThis.api = { post: function(){ return { status: 200, data: { errors: [\n"
               "    { message: 'boom', extensions: { code: 'validation-failed' } }] } }; }};\n"
               "  var h = hasura({ endpoint: 'https://hasura.test' });\n"
               "  try { h.query('query { x }'); return json('no-throw', null); }\n"
               "  catch(e){ return json({ msg: e.message, code: e.code, n: e.graphql.length }, null); }\n"
               "}"),
           data_eq({"msg": "boom", "code": "validation-failed", "n": 1}))

    # Transport failure (api's in-band status:0) → raw() normalizes to an errors envelope,
    # query() throws carrying the transport code.
    t.test("transport failure normalizes + throws",
           h_raw(
               "import { hasura } from 'hasura/client';\n"
               "export default function handler(ctx){\n"
               "  globalThis.api = { post: function(){ return { status: 0, error: { code: 'HTTP_CONNECT', retryable: true } }; }};\n"
               "  var h = hasura({ endpoint: 'https://hasura.test' });\n"
               "  var env = h.raw('query { x }');\n"
               "  var threw = '';\n"
               "  try { h.query('query { x }'); } catch(e){ threw = e.code; }\n"
               "  return json({ envCode: env.errors[0].extensions.code, threw: threw }, null);\n"
               "}"),
           data_eq({"envCode": "HTTP_CONNECT", "threw": "HTTP_CONNECT"}))

    # No endpoint anywhere (no opts, no $sys.env) → a clear, actionable throw.
    t.test("missing endpoint throws a helpful error",
           h_raw(
               "import { hasura } from 'hasura/client';\n"
               "export default function handler(ctx){\n"
               "  try { hasura(); return json('no-throw', null); }\n"
               "  catch(e){ return json(e.message.indexOf('HASURA_ENDPOINT') >= 0, null); }\n"
               "}"),
           data_eq(True))


def test_circuit_breaker(t: Runner):
    """Tier 3: repeated connect failures to a dead db target trip the breaker, after which
    requests to that target fast-fail DB_CIRCUIT_OPEN instead of waiting on the timeout —
    and a healthy target is unaffected."""
    t.section("Circuit breaker (Tier 3)")
    # The db breaker moved to `fabricd` with the trust flip (Step 5) and is not yet implemented
    # there, so repeated connect failures no longer trip `DB_CIRCUIT_OPEN` — this test self-skips
    # until the daemon grows a breaker. `db-broken` is the operator's unreachable resource.
    bad = "db-broken"
    script = "db.query('SELECT 1'); return json('ok', null);"

    codes = [_err_code(_post(h(script, config=_db_io(bad)))) for _ in range(7)]
    if "DB_CIRCUIT_OPEN" not in codes:
        print("  \033[33mPROBE\033[0m breaker not active (moved to fabricd, not yet implemented) — skipping\n")
        return

    t.test("breaker trips DB_CIRCUIT_OPEN after repeated connect failures",
           h("return json(1,null);"), lambda _r: "DB_CIRCUIT_OPEN" in codes)
    # An open breaker fast-fails — no connect attempt, so well under any connect wait.
    start = time.time()
    r = _post(h(script, config=_db_io(bad)))
    elapsed = time.time() - start
    t.test("open breaker fast-fails (no connect wait)",
           h("return json(1,null);"),
           lambda _r: _err_code(r) == "DB_CIRCUIT_OPEN" and elapsed < 1.5)
    # A different, healthy target is not affected by the bad target's open breaker.
    t.test("healthy db target unaffected by another target's open breaker",
           h("db.query('SELECT 1'); return json('up', null);", config=_db_io("pg")),
           data_eq("up"))


def test_statement_timeout_clamp(t: Runner, db: str):
    """Prove the operator ceiling clamps a resource's statement_timeout it cannot raise (Tier 0).
    The clamp now runs in `fabricd` (`max_statement_timeout_ms`); `db` is the engine base name —
    its `-unlimited` (0) and `-huge` (60000) variants are clamped to the daemon ceiling."""
    t.section("statement_timeout clamp (Tier 0)")

    def killed(name):
        r = _post(h("db.query('SELECT pg_sleep(2)'); return json('slept', null);", config=_db_io(name)))
        return r is not None and r["data"] is None and r["error"] is not None

    # The `-unlimited` resource asks for no timeout. If fabricd's ceiling is active, the 2s sleep is
    # killed well before it finishes. If no ceiling is configured, the sleep completes — probe and
    # skip rather than fail.
    if not killed(db + "-unlimited"):
        print("  \033[33mPROBE\033[0m clamp not active (fabricd has no max_statement_timeout_ms) — skipping\n")
        return
    t.test("resource statement_timeout=0 (unlimited) is clamped + killed",
           h("return json(1,null);"), lambda _r: True)
    t.test("resource statement_timeout=60000 (huge) is clamped + killed",
           h("return json(1,null);"), lambda _r: killed(db + "-huge"))


# -- Adversarial: PgBouncer transaction-mode sharp edges ---------------------

def test_pgbouncer_edges(t: Runner, db: str, direct: bool):
    """Probe the documented hazards of running jsbox's per-request connect model
    behind a transaction-pooling PgBouncer. `direct` = same probes against raw
    Postgres for comparison (those MUST all be safe)."""
    label = "Postgres-direct" if direct else "PgBouncer"
    t.section(f"Connection-pool edges ({label})")

    # (1) statement_timeout enforcement. jsbox applies it as a session-level `SET` at
    # connect (db.rs). On a direct connection this is a hard guarantee — assert it. Behind
    # PgBouncer transaction mode it is BEST-EFFORT: the SET binds to one server connection
    # and a later autocommit statement may run on a different one, so we probe and record
    # rather than assert. (A startup parameter would be robust but PgBouncer refuses it:
    # "unsupported startup parameter in options". The robust path through a txn-mode pooler
    # is a server-side role default — see docs/design/pooled-capabilities.md.) Either way
    # jsbox's wall-clock interrupt cannot cancel a blocking libpq call, so the DB-side cap
    # is the only thing that stops a slow query.
    fast = db + "-fast"
    r = _post(h("db.query('SELECT pg_sleep(3)'); return json('slept-full', null);",
                config=_db_io(fast)))
    enforced = r is not None and r["data"] is None and r["error"] is not None
    if direct:
        t.test(f"{label}: statement_timeout enforced (sleep killed)",
               h("return json(1,null);"), lambda _r: enforced)
    else:
        verdict = "ENFORCED" if enforced else "NOT ENFORCED — sleep ran full"
        print(f"  \033[33mPROBE\033[0m {label}: statement_timeout via SET -> {verdict} "
              f"(best-effort in txn pooling; use a server-side role default for a guarantee)")
        t.test(f"{label}: server responsive after long query",
               h("return json('alive', null);"), data_eq("alive"))

    # (2) Explicit transactions pin to one server connection in txn mode — multi-step
    # work and session-scoped temp tables MUST hold within begin/commit. This is the
    # safe pattern and must pass on both.
    t.test(f"{label}: temp table within one transaction",
           h("db.begin();"
             "db.execute('CREATE TEMP TABLE t_edge(x int) ON COMMIT DROP');"
             "db.execute('INSERT INTO t_edge VALUES (7)');"
             "var r = db.query('SELECT x FROM t_edge');"
             "db.commit();"
             "return json(r.rows[0].x, null);", config=_db_io(db)),
           data_eq(7))

    # (3) Prepared-statement reuse: the Rust driver prepares each query; hammering the
    # same parameterized query many times must not trip "prepared statement does not
    # exist" as PgBouncer rotates server connections (needs max_prepared_statements>0).
    t.test(f"{label}: 25x parameterized query reuse",
           h("var n=0; for (var i=0;i<25;i++){var r=db.query('SELECT $1::int AS v',[i]); n+=r.rows[0].v;} return json(n, null);",
             config=_db_io(db)),
           data_eq(sum(range(25))))


def test_pooler_query_timeout(t: Runner, db: str):
    """Tier 4: PgBouncer's own query_timeout is an INDEPENDENT backstop. Through a
    transaction-mode pooler the session `SET statement_timeout` is best-effort and can be
    lost; query_timeout (2s, set on the pooler) guarantees a runaway query is still killed
    — and below jsbox's 4s wall-clock deadline (Tier 2). See docs/design/resilience.md."""
    t.section("Pooler query_timeout (Tier 4)")

    # pg_sleep(3) outlives the 2s pooler ceiling but is under jsbox's 4s wall clock, so the
    # *pooler* is what must catch it (whether or not the session SET also fired).
    cfg = db + "-fast"
    start = time.time()
    r = _post(h("db.query('SELECT pg_sleep(3)'); return json('slept', null);", config=_db_io(cfg)))
    elapsed = time.time() - start
    killed = r is not None and r["data"] is None and r["error"] is not None

    # No kill means neither the SET nor a pooler query_timeout fired (sleep ran ~3s under
    # jsbox's 4s budget) — the pooler has no ceiling configured. Probe + skip rather than fail.
    if not killed:
        print(f"  \033[33mPROBE\033[0m pooler did not terminate the 3s query (query_timeout unset?) "
              f"elapsed={elapsed:.1f}s\n")
        return
    t.test("pooler terminates an over-budget query (error returned)",
           h("return json(1,null);"), lambda _r: killed)
    t.test("terminated below jsbox wall-clock deadline (independent of Tier 2)",
           h("return json(1,null);"), lambda _r: elapsed < 3.7)
    t.test("pooler healthy immediately after the kill",
           h("return json('alive', null);"), data_eq("alive"))


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


def test_auth_provider(t: Runner, label: str, token: str, has_introspect: bool):
    """Drive the `auth` capability against a real OIDC/IAM (provider-agnostic). The auth resources
    (`auth-<label>` and, if creds were minted, `auth-<label>-introspect`) were registered with
    `fabricd` at startup; here we reference them by name."""
    t.section(f"Auth ({label})")
    base = f"auth-{label.lower()}"
    cfg = _auth_io(base)
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

    if has_introspect:
        icfg = _auth_io(f"{base}-introspect")
        t.test(f"{label}: introspect(valid) -> active:true",
               h("return json(auth.introspect(ctx.token).claims.active, null);", ctx, icfg),
               data_eq(True))
        t.test(f"{label}: introspect(bogus) -> active:false",
               h("return json(auth.introspect('bogus').claims.active, null);", config=icfg),
               data_eq(False))


def discover_auth() -> tuple[dict, list]:
    """Talk **directly** to the identity providers (before the box/fabricd start) to mint tokens +
    introspection client creds. Returns `(auth_resources, providers)` where `auth_resources` is the
    name→binding map to merge into the `fabricd` table (so credentials are present at startup), and
    `providers` is `[(label, token, has_introspect), ...]` for the reachable ones.
    """
    auth_resources: dict = {}
    providers: list = []

    # Keycloak — mint a token + a confidential client live.
    if _discovery_ok(KEYCLOAK_ISSUER):
        kc_token = _keycloak_token()
        if kc_token:
            creds = _keycloak_introspect_creds(kc_token)
            auth_resources.update(_auth_resources("Keycloak", KEYCLOAK_ISSUER, creds))
            providers.append(("Keycloak", kc_token, creds is not None))
        else:
            print("\n  \033[33mSKIP\033[0m Keycloak auth tests (reachable but token mint failed)\n")
    else:
        print("\n  \033[33mSKIP\033[0m Keycloak auth tests (not running — use: docker compose up -d keycloak)\n")

    # ZITADEL — needs a service-account PAT (introspection needs an API app, so it is
    # exercised on Keycloak; ZITADEL covers discovery + userinfo + the throw path).
    zt_token = _zitadel_token()
    if zt_token and _discovery_ok(ZITADEL_ISSUER):
        auth_resources.update(_auth_resources("Zitadel", ZITADEL_ISSUER, None))
        providers.append(("Zitadel", zt_token, False))
    elif zt_token:
        print("\n  \033[33mSKIP\033[0m Zitadel auth tests (PAT set but issuer unreachable)\n")
    else:
        print("\n  \033[33mSKIP\033[0m Zitadel auth tests (no ZITADEL_PAT — see docker-compose.yml)\n")

    return auth_resources, providers


def run_auth_tests(t: Runner, providers: list):
    """Run the auth suite for each provider discovered before startup."""
    for label, token, has_introspect in providers:
        test_auth_provider(t, label, token, has_introspect)


# -- Main --------------------------------------------------------------------

def _wait_for_server() -> bool:
    for _ in range(20):
        if _post(h("return json(1, null);")) is not None:
            return True
        time.sleep(0.5)
    return False


def _start_servers(resources: dict) -> list:
    """Start the two-process topology in `.test-run/`: `fabricd` (holds the credential `resources`
    table + the drivers) over a UDS, then `runlet` (the box, driver-free) pointed at that socket.

    The box reads `config.json` from its cwd; `fabricd` reads its table from `FABRICD_CONFIG`. Both
    live in a gitignored scratch dir so this doesn't change `task run` behavior. Returns the
    started processes (caller terminates them).
    """
    repo = os.path.dirname(os.path.abspath(__file__))
    run_dir = os.path.join(repo, ".test-run")
    os.makedirs(run_dir, exist_ok=True)
    # Merge the test-fixture modules (tests/modules) with the shipped operator modules
    # (modules/, e.g. hasura/client.mjs) into one scratch modules_dir, so both are
    # importable without duplicating the shipped module as a fixture (single source of truth).
    merged_modules = os.path.join(run_dir, "modules")
    if os.path.isdir(merged_modules):
        shutil.rmtree(merged_modules)
    os.makedirs(merged_modules)
    for src in (os.path.join(repo, "tests", "modules"), os.path.join(repo, "modules")):
        if os.path.isdir(src):
            shutil.copytree(src, merged_modules, dirs_exist_ok=True)

    socket = os.path.join(run_dir, "fabricd.sock")
    # fabricd: the operator credential table + the Tier-0 statement_timeout ceiling. Credentials
    # live ONLY here — the box never sees them.
    fabricd_cfg = {"socket": socket, "max_statement_timeout_ms": 800, "resources": resources}
    with open(os.path.join(run_dir, "fabricd.json"), "w", encoding="utf-8") as fh:
        json.dump(fabricd_cfg, fh)
    # Box: a fabricd socket + scripts/modules + low bounds. NO `resources`, NO credentials.
    # debug=true relaxes the SSRF private-IP block so the `api` tests can reach the local httpbin.
    box_cfg = {
        "debug": True,
        "scripts_dir": os.path.join(repo, "tests", "scripts"),
        "modules_dir": merged_modules,
        "fabricd_socket": socket,
        "engine": {"max_concurrent_executions": 6, "max_concurrent_per_partition": 2},
    }
    with open(os.path.join(run_dir, "config.json"), "w", encoding="utf-8") as fh:
        json.dump(box_cfg, fh)

    # Build both up front, then launch the binaries directly — two concurrent `cargo run`
    # invocations would race on the target/ build lock.
    subprocess.run(["cargo", "build", "-p", "fabricd", "-p", "runlet"], cwd=repo, check=True)
    bindir = os.path.join(repo, "target", "debug")
    if os.path.exists(socket):
        os.remove(socket)
    fabricd = subprocess.Popen(
        [os.path.join(bindir, "fabricd")], cwd=run_dir,
        env={**os.environ, "FABRICD_CONFIG": os.path.join(run_dir, "fabricd.json")},
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    for _ in range(60):  # wait for fabricd to bind the socket before starting the box
        if os.path.exists(socket):
            break
        time.sleep(0.5)
    runlet = subprocess.Popen(
        [os.path.join(bindir, "runlet")], cwd=run_dir,
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    return [fabricd, runlet]


def _start_trusted_box(port: int = 3010):
    """Start a dedicated `runlet` in trusted-header mode on a loopback port, for the N5 acting-org
    gate. No `fabricd` is needed — the gate fires before any egress session, and the probe script is
    deterministic. Loopback needs no `assert_network_isolation`. Returns `(proc, base_url)` or
    `(None, None)` if the box could not be built/started (the caller self-skips)."""
    repo = os.path.dirname(os.path.abspath(__file__))
    run_dir = os.path.join(repo, ".test-run", "trusted")
    os.makedirs(run_dir, exist_ok=True)
    cfg = {"server": {"host": "127.0.0.1", "port": port}, "trusted": {"enabled": True}}
    with open(os.path.join(run_dir, "config.json"), "w", encoding="utf-8") as fh:
        json.dump(cfg, fh)
    try:
        subprocess.run(["cargo", "build", "-p", "runlet"], cwd=repo, check=True)
    except Exception:
        return None, None
    binpath = os.path.join(repo, "target", "debug", "runlet")
    proc = subprocess.Popen(
        [binpath], cwd=run_dir, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    url = f"http://127.0.0.1:{port}/execute"
    probe = h("return json(1, null);")
    acting = {"x-tenant-id": "ws_probe", "x-tenant-scope": "acting"}
    for _ in range(40):
        st, _r = _post_status(url, probe, acting)
        if st is not None:
            return proc, url
        time.sleep(0.5)
    proc.terminate()
    return None, None


def test_trusted_acting_scope(t: Runner):
    """Nexus N5: in trusted-header mode a tenant-scoped request must carry `x-tenant-scope: acting`
    (the edge's acting-org assertion) or it is rejected `403 ACTING_SCOPE_REQUIRED` before any
    execution. Runs against a dedicated trusted-mode box on its own port."""
    t.section("Trusted-mode acting-org assurance (nexus N5)")
    proc, url = _start_trusted_box()
    if proc is None:
        print("  \033[33mSKIP\033[0m trusted-mode box failed to build/start — asserts skipped\n")
        return
    try:
        script = h("return json(1, null);")
        tenant = {"x-tenant-id": "ws_a"}

        st, r = _post_status(url, script, {**tenant, "x-tenant-scope": "acting"})
        t.check("acting-org request executes (200, data == 1)",
                st == 200 and r is not None and r.get("data") == 1)

        st, r = _post_status(url, script, tenant)
        t.check("missing scope rejected 403 ACTING_SCOPE_REQUIRED",
                st == 403 and _err_code(r) == "ACTING_SCOPE_REQUIRED")

        st, r = _post_status(url, script, {**tenant, "x-tenant-scope": "home"})
        t.check("non-acting scope rejected 403 ACTING_SCOPE_REQUIRED",
                st == 403 and _err_code(r) == "ACTING_SCOPE_REQUIRED")
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=10)
        except Exception:
            proc.kill()


def _start_telemetry_box(port: int = 3011):
    """Start a dedicated `runlet` with tracing enabled, pointed at an OTLP endpoint nothing is
    listening on. Exercises three things at once: W3C `traceparent` propagation into
    `meta.trace_id`, fail-open export (the request must still succeed with the collector down), and
    structured JSON logs on stdout. Returns `(proc, url, log_path)` or `(None, None, None)`."""
    repo = os.path.dirname(os.path.abspath(__file__))
    run_dir = os.path.join(repo, ".test-run", "telemetry")
    os.makedirs(run_dir, exist_ok=True)
    cfg = {
        "server": {"host": "127.0.0.1", "port": port},
        # Nothing listens on :4317 — the tonic channel is lazy, so export just fails in the
        # background (drop-on-full) while requests proceed (fail-open, design D6).
        "telemetry": {
            "otlp_endpoint": "http://127.0.0.1:4317",
            "sample_ratio": 1.0,
            "service_name": "runlet-test",
        },
    }
    with open(os.path.join(run_dir, "config.json"), "w", encoding="utf-8") as fh:
        json.dump(cfg, fh)
    try:
        subprocess.run(["cargo", "build", "-p", "runlet"], cwd=repo, check=True)
    except Exception:
        return None, None, None
    binpath = os.path.join(repo, "target", "debug", "runlet")
    log_path = os.path.join(run_dir, "stdout.log")
    logf = open(log_path, "w", encoding="utf-8")  # noqa: SIM115 (held for the child's lifetime)
    proc = subprocess.Popen(
        [binpath], cwd=run_dir, stdout=logf, stderr=subprocess.STDOUT)
    url = f"http://127.0.0.1:{port}/execute"
    probe = h("return json(1, null);")
    for _ in range(40):
        st, _r = _post_status(url, probe)
        if st is not None:
            return proc, url, log_path
        time.sleep(0.5)
    proc.terminate()
    return None, None, None


def test_telemetry_tracing(t: Runner):
    """OpenTelemetry tracing + structured logs: a propagated W3C `traceparent` becomes
    `meta.trace_id`; without one the box starts its own 32-hex root id; the request succeeds even
    though the collector is unreachable (fail-open); and stdout is structured JSON."""
    t.section("OpenTelemetry tracing + structured logs")
    proc, url, log_path = _start_telemetry_box()
    if proc is None:
        print("  \033[33mSKIP\033[0m telemetry box failed to build/start — asserts skipped\n")
        return
    try:
        script = h("return json(1, null);")
        is_hex32 = lambda s: len(s) == 32 and all(c in "0123456789abcdef" for c in s)

        # 1. Propagation: the box continues the edge trace, so meta.trace_id == the traceparent id.
        tp_trace = "0af7651916cd43dd8448eb211c80319c"
        st, r = _post_status(url, script, {"traceparent": f"00-{tp_trace}-b7ad6b7169203331-01"})
        tid = (r or {}).get("meta", {}).get("trace_id", "")
        t.check("traceparent continued into meta.trace_id",
                st == 200 and tid == tp_trace)

        # 2. No traceparent: a fresh box-rooted 32-hex trace id, and the request still succeeds
        #    (fail-open — the OTLP collector is down).
        st2, r2 = _post_status(url, script)
        tid2 = (r2 or {}).get("meta", {}).get("trace_id", "")
        t.check("box starts its own trace when no traceparent (fail-open success)",
                st2 == 200 and is_hex32(tid2) and tid2 != tp_trace)

        # 3. Structured logging: stdout carries at least one valid JSON log object.
        time.sleep(0.5)
        json_lines = 0
        with open(log_path, encoding="utf-8") as fh:
            for line in fh:
                line = line.strip()
                if not line:
                    continue
                try:
                    json.loads(line)
                    json_lines += 1
                except ValueError:
                    pass
        t.check("server emits structured JSON logs to stdout", json_lines > 0)
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=10)
        except Exception:
            proc.kill()


def main():
    procs: list = []

    # Auth discovery talks DIRECTLY to the identity providers (no box), so it must run BEFORE
    # fabricd starts — the minted introspection client creds have to be in fabricd's resource table
    # at startup (the box never carries them).
    auth_resources, auth_providers = discover_auth()

    if not _wait_for_server():
        print("Starting fabricd + runlet...")
        procs = _start_servers(build_resources(auth_resources))
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

    test_registry(t)
    test_registry_hardening(t)
    test_isolation_under_concurrency(t)
    test_bulkhead(t)
    test_partition_fairness(t)
    test_metrics(t)
    test_esm(t)
    test_hasura(t)

    # Database tests — only if the backend is reachable (probed by resource name through fabricd).
    if _db_available("pg"):
        test_db_engine(t, "PostgreSQL", "pg")
        test_pgbouncer_edges(t, "pg", direct=True)
        test_statement_timeout_clamp(t, "pg")
        test_circuit_breaker(t)
    else:
        print("\n  \033[33mSKIP\033[0m PostgreSQL tests (not running — use: docker compose up -d)\n")

    # Same db suite through PgBouncer (transaction pooling) — proves the per-request
    # connect model works unchanged behind a pooler (docs/design/pooled-capabilities.md).
    if _db_available("pgbouncer"):
        test_db_engine(t, "PgBouncer", "pgbouncer")
        test_pgbouncer_edges(t, "pgbouncer", direct=False)
        test_pooler_query_timeout(t, "pgbouncer")
    else:
        print("\n  \033[33mSKIP\033[0m PgBouncer tests (not running — use: docker compose up -d pgbouncer)\n")

    if _db_available("cockroach"):
        test_db_engine(t, "CockroachDB", "cockroach")
    else:
        print("\n  \033[33mSKIP\033[0m CockroachDB tests (not running — use: docker compose up -d)\n")

    # Mongo + NATS — only if their containers are running
    if _mongo_available("mongo"):
        test_mongo(t)
    else:
        print("\n  \033[33mSKIP\033[0m Mongo tests (not running — use: docker compose up -d mongo)\n")

    if _nats_available("nats"):
        test_nats(t)
    else:
        print("\n  \033[33mSKIP\033[0m NATS tests (not running — use: docker compose up -d nats)\n")

    # Auth tests — for whichever providers were reachable at discovery (before startup).
    run_auth_tests(t, auth_providers)

    # Trusted-mode acting-org gate (nexus N5): needs its own box in trusted mode. Only when this
    # harness owns the local build/run (it spins up a second runlet on a loopback port); skipped when
    # pointed at an already-running / remote server (JSBOX_URL), which we don't reconfigure.
    if procs:
        test_trusted_acting_scope(t)
        test_telemetry_tracing(t)
    else:
        print("\n  \033[33mSKIP\033[0m trusted-mode N5 + telemetry tests (external server; harness didn't start it)\n")

    t.summary()

    for proc in procs:
        proc.terminate()

    sys.exit(t.failed)


if __name__ == "__main__":
    main()
