#!/usr/bin/env python3
"""Integration tests for jsbox."""

import json
import subprocess
import sys
import time
import urllib.error
import urllib.request

BASE_URL = "http://127.0.0.1:3000"


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
    """Assert data == expected and no errors."""
    return lambda r: r["data"] == expected and r["errors"] is None


def data_is_none():
    return lambda r: r["data"] is None


def has_error():
    return lambda r: r["errors"] is not None


def error_contains(text: str):
    return lambda r: r["errors"] is not None and text in str(r["errors"])


def data_none_with_error():
    return lambda r: r["data"] is None and r["errors"] is not None


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
           lambda r: r["errors"]["messages"][0] == "name required")
    t.test("custom error object",
           h('return json(null, {code: 400, detail: "bad input"});'),
           lambda r: r["errors"]["code"] == 400 and r["errors"]["detail"] == "bad input")
    t.test("data with warnings",
           h('return json({status: "ok"}, {warnings: ["low battery"]});'),
           lambda r: r["data"]["status"] == "ok" and r["errors"]["warnings"][0] == "low battery")


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
    t.test("null data and errors",     h("return json(null, null);"),       lambda r: r["data"] is None and r["errors"] is None)


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

    t.summary()

    if server_proc:
        server_proc.terminate()

    sys.exit(t.failed)


if __name__ == "__main__":
    main()
