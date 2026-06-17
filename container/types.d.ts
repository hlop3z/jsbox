/**
 * Type definitions for the **jsbox** sandbox.
 *
 * These describe the globals available inside the `handler(ctx)` function you
 * `POST /execute`. Keep this file beside your script and the shipped
 * `tsconfig.json` for editor autocomplete and type-checking out of the box.
 *
 * The bundled `tsconfig.json` already enables `checkJs`, so any top-level `.js`
 * script in this folder is checked automatically — just write your handler:
 * ```js
 * /** @type {Handler} *\/
 * function handler(ctx) {
 *   return json({ hi: ctx.name }, null);
 * }
 * ```
 * (No `/// <reference>` or `// @ts-check` needed; the tsconfig wires it up.)
 *
 * @remarks
 * **Capabilities are opt-in.** `api`, `db`, `mail`, and `s3` exist **only** when
 * the matching config block (`config.api` / `config.db` / `config.mail` /
 * `config.s3`) is present in the request — otherwise the global is `undefined`
 * (e.g. `typeof mail === "undefined"`). They are declared here as always-present
 * for convenient autocomplete; guard with `typeof` if a capability is optional.
 * `json`, `$`, `Decimal`, and `$sys.crypto` / `$sys.date` are pure and **always**
 * available; `$sys.env` / `$sys.secrets` populate only when `config.sys` is set.
 *
 * `eval` and `Proxy` are removed before your `handler` runs.
 */

// ─────────────────────────────────────────────────────────────────────────────
// Response envelope
// ─────────────────────────────────────────────────────────────────────────────

/**
 * Builds the `{ data, error }` envelope your `handler` must return. The server
 * attaches `meta` and replies with `{ data, error, meta }`.
 *
 * Pass `null` for whichever side doesn't apply.
 *
 * @param data  The success payload (any JSON-serializable value, or `null`).
 * @param error The error payload (any JSON-serializable value, or `null`).
 *
 * @example
 * return json({ ok: true }, null);     // success
 * @example
 * return json(null, { message: "bad input" }); // failure
 */
declare function json(data: unknown, error?: unknown): string;

/**
 * The function the sandbox calls. Define `function handler(ctx) { ... }` in your
 * script; it receives the request's `context` and must return {@link json}`(...)`.
 *
 * @example
 * /** @type {Handler} *\/
 * function handler(ctx) {
 *   return json({ hello: ctx.name }, null);
 * }
 */
type Handler = (ctx: any) => string;

// ─────────────────────────────────────────────────────────────────────────────
// `$` / `Decimal` — exact decimal math (always available)
// ─────────────────────────────────────────────────────────────────────────────

/** A value accepted anywhere a decimal is expected. */
type DecimalInput = number | string | Decimal;

/**
 * An exact, arbitrary-precision decimal. JavaScript has no operator overloading,
 * so arithmetic is method-based and **immutable** — every operation returns a new
 * `Decimal`. Backed by the same engine that decodes Postgres `NUMERIC`, so it
 * round-trips DB decimals without precision loss.
 *
 * @example
 * const total = $("0.1").add("0.2");   // exact 0.3, not 0.30000000000000004
 * total.toString();                    // "0.3"
 */
interface Decimal {
  /** Returns `this + other`. */
  add(other: DecimalInput): Decimal;
  /** Returns `this - other`. */
  sub(other: DecimalInput): Decimal;
  /** Returns `this * other`. */
  mul(other: DecimalInput): Decimal;
  /** Returns `this / other`. */
  div(other: DecimalInput): Decimal;
  /** Returns `-this`. */
  neg(): Decimal;
  /** Returns `|this|`. */
  abs(): Decimal;
  /** Rounds to `places` decimal places (default `0`), half-away-from-zero. */
  round(places?: number): Decimal;
  /**
   * Converts major units to integer minor units: `this * 10^places`, rounded
   * half-away-from-zero to a whole number. `places` is the count of minor-unit
   * digits and defaults to `2` (cents) — pass `0` for yen, `3` for dinars.
   * @example $("19.99").toCents();   // 1999
   * @example $("1.005").toCents();   // 101  (sub-cent rounds half-up)
   */
  toCents(places?: number): Decimal;
  /**
   * Converts integer minor units back to major units: `this / 10^places`, fixed to
   * `places` decimal places. `places` defaults to `2` (cents).
   * @example $(1999).fromCents(); // "19.99"
   */
  fromCents(places?: number): Decimal;
  /** Compares: returns `-1` if `this < other`, `0` if equal, `1` if greater. */
  cmp(other: DecimalInput): number;
  /** `this === other`. */
  eq(other: DecimalInput): boolean;
  /** `this < other`. */
  lt(other: DecimalInput): boolean;
  /** `this <= other`. */
  lte(other: DecimalInput): boolean;
  /** `this > other`. */
  gt(other: DecimalInput): boolean;
  /** `this >= other`. */
  gte(other: DecimalInput): boolean;
  /** `true` if the value is exactly zero. */
  isZero(): boolean;
  /** `true` if the value is less than zero. */
  isNegative(): boolean;
  /** The exact value as a decimal string (e.g. `"19.99"`). */
  toString(): string;
  /** The value as a JS `number` — may lose precision for large/long decimals. */
  toNumber(): number;
  /** Serializes as the exact string value inside {@link json} / `JSON.stringify`. */
  toJSON(): string;
}

/**
 * Creates a {@link Decimal} from a number, string, or another `Decimal`.
 * `$` and `Decimal` are the same function.
 *
 * @example
 * const price = $("19.99").mul(3).round(2); // "59.97"
 */
interface DecimalFactory {
  (value?: DecimalInput): Decimal;
}

/** Exact-decimal factory. Alias of {@link Decimal}. Always available. */
declare const $: DecimalFactory;
/** Exact-decimal factory. Alias of {@link $}. Always available. */
declare const Decimal: DecimalFactory;

// ─────────────────────────────────────────────────────────────────────────────
// `api` — SSRF-guarded HTTP client (present when `config.api` is set)
// ─────────────────────────────────────────────────────────────────────────────

/** Request/response header map. */
interface HttpHeaders {
  [name: string]: string;
}

/** Query-string parameters for `api.get` (values are stringified). */
interface QueryParams {
  [name: string]: string | number | boolean;
}

/** Result of an `api.*` call. */
interface ApiResponse<T = any> {
  /** HTTP status code, or `0` if the request failed before a response (transport error). */
  status: number;
  /** Parsed JSON body (raw string if not JSON). Present on any HTTP response; absent on a transport failure. */
  data?: T;
  /**
   * In-band transport error — present only when `status === 0` (the request never reached
   * a response). `api` never throws (§13): inspect this inline instead of `try/catch`.
   */
  error?: ApiTransportError;
}

/** Structured transport error on an `api.*` call (`status: 0`). */
interface ApiTransportError {
  /** Stable code: `HTTP_TIMEOUT` | `HTTP_CONNECT` | `HTTP_SSRF_BLOCKED` | `HTTP_BODY_TOO_LARGE` | `HTTP_OP_LIMIT` | `HTTP_ERROR`. */
  code: string;
  /** `true` ⇒ a retry may succeed (transient). */
  retryable: boolean;
  /** Who should act: `"operator"` (network/upstream) or `"developer"` (e.g. blocked host). */
  owner: string;
  /** Always `"api"`. */
  source: string;
  /** Human-safe cause. */
  message?: string;
}

/**
 * HTTP client whose targets are **script-controlled**, so it is SSRF-guarded:
 * only `http`/`https`, the host must be in `config.api.allowed_hosts`, and
 * private/internal IPs are blocked (re-validated across redirects).
 */
interface HttpClient {
  /**
   * `GET url`, with optional query params appended.
   * @example api.get("https://api.example.com/items", { page: 2 });
   */
  get<T = any>(
    url: string,
    params?: QueryParams,
    headers?: HttpHeaders,
  ): ApiResponse<T>;
  /** `POST url` with a JSON `body`. */
  post<T = any>(
    url: string,
    body?: unknown,
    headers?: HttpHeaders,
  ): ApiResponse<T>;
  /** `PUT url` with a JSON `body`. */
  put<T = any>(
    url: string,
    body?: unknown,
    headers?: HttpHeaders,
  ): ApiResponse<T>;
  /** `PATCH url` with a JSON `body`. */
  patch<T = any>(
    url: string,
    body?: unknown,
    headers?: HttpHeaders,
  ): ApiResponse<T>;
  /** `DELETE url`. */
  delete<T = any>(url: string, headers?: HttpHeaders): ApiResponse<T>;
}

/** HTTP client. Present only when `config.api` is supplied. */
declare const api: HttpClient;

// ─────────────────────────────────────────────────────────────────────────────
// `db` — Postgres / CockroachDB (present when `config.db` is set)
// ─────────────────────────────────────────────────────────────────────────────

/**
 * A single column value. Values that don't fit a JS number exactly come back as
 * **strings** — `BIGINT` (INT8), `NUMERIC`/`DECIMAL`, `UUID`, timestamps, etc.
 * INT2/INT4 and floats are numbers. Use {@link $} for exact math on string decimals.
 */
type DbValue = string | number | boolean | null;

/** A result row, keyed by column name. */
interface DbRow {
  [column: string]: DbValue;
}

/** Result of a `db.query` / `db.execute` call. */
interface DbResult {
  /** Column names, in selection order. */
  columns: string[];
  /** The rows returned (capped by the server's `max_rows`). */
  rows: DbRow[];
  /** Number of rows in {@link rows}. */
  row_count: number;
  /** `true` if rows were dropped because the result hit `max_rows`. */
  truncated: boolean;
}

/**
 * SQL client over an **operator-supplied** connection (`config.db`), so it is
 * trusted — no SSRF guard. Parameters are bound with `$1`, `$2`, … (never string
 * interpolation). `query` and `execute` are equivalent; use the name that reads best.
 */
interface Db {
  /**
   * Runs a SQL statement and returns its rows.
   * @example db.query("SELECT id, email FROM users WHERE id = $1", [ctx.id]);
   */
  query(sql: string, params?: unknown[]): DbResult;
  /**
   * Runs a SQL statement (typically a write) and returns the result.
   * @example db.execute("UPDATE users SET seen = now() WHERE id = $1", [ctx.id]);
   */
  execute(sql: string, params?: unknown[]): DbResult;
  /** Begins a transaction. */
  begin(): void;
  /** Commits the current transaction. */
  commit(): void;
  /** Rolls back the current transaction. */
  rollback(): void;
}

/** SQL client. Present only when `config.db` is supplied. */
declare const db: Db;

// ─────────────────────────────────────────────────────────────────────────────
// `mail` — SMTP send (present when `config.mail` is set)
// ─────────────────────────────────────────────────────────────────────────────

/** Options for {@link Mail.send}. A single address or a list is accepted. */
interface MailOptions {
  /** Sender address. Defaults to `config.mail.from` if omitted. */
  from?: string;
  /** Recipient(s). */
  to?: string | string[];
  /** Carbon-copy recipient(s). */
  cc?: string | string[];
  /** Blind-carbon-copy recipient(s). */
  bcc?: string | string[];
  /** `Reply-To` address. */
  reply_to?: string;
  /** Subject line. */
  subject?: string;
  /** Plain-text body. */
  text?: string;
  /** HTML body. */
  html?: string;
}

/** Result of {@link Mail.send}. */
interface MailResult {
  /** `true` if the SMTP server returned a positive (2xx) reply. */
  accepted: boolean;
  /** The SMTP server's response line. */
  response: string;
}

/**
 * SMTP mailer over an **operator-supplied** relay (`config.mail`), so it is
 * trusted — no SSRF guard. Throws on a send failure.
 */
interface Mail {
  /**
   * Sends one email.
   * @example mail.send({ to: ctx.email, subject: "Hi", text: "Hello!" });
   */
  send(opts: MailOptions): MailResult;
}

/** SMTP mailer. Present only when `config.mail` is supplied. */
declare const mail: Mail;

// ─────────────────────────────────────────────────────────────────────────────
// `s3` — presigned URLs + folder usage (present when `config.s3` is set)
// ─────────────────────────────────────────────────────────────────────────────

/** HTTP method a presigned URL is signed for. */
type S3Method = "PUT" | "GET" | "HEAD" | "DELETE";

/** Options for {@link S3.upload_url} / {@link S3.download_url}. */
interface S3PresignOptions {
  /** Object key (path within the bucket), e.g. `"uploads/photo.jpg"`. */
  key: string;
  /** Link lifetime in seconds. Defaults to `config.s3.expires`; capped at `max_expires`. */
  expires?: number;
}

/** Options for the general {@link S3.sign_url}. */
interface S3PresignGeneralOptions extends S3PresignOptions {
  /** HTTP method to sign for. Defaults to `"PUT"`. */
  method?: S3Method;
}

/** Result of {@link S3.sign_url} / {@link S3.upload_url} / {@link S3.download_url}. */
interface S3PresignResult {
  /** The signed URL the browser uses directly. */
  url: string;
  /** The method the URL is signed for. */
  method: S3Method;
  /** The link's lifetime in seconds. */
  expires: number;
}

/** Options for {@link S3.upload_form}. */
interface S3PresignPostOptions {
  /** Object key the upload will be stored under. */
  key: string;
  /** Link lifetime in seconds. Defaults to `config.s3.expires`. */
  expires?: number;
}

/**
 * Result of {@link S3.upload_form} — a browser POST policy whose size limit the
 * object store enforces (the cap comes from `config.s3.max_upload_size`).
 */
interface S3PresignPostResult {
  /** The POST target URL. */
  url: string;
  /** Form fields to send before the `file` part. */
  fields: { [field: string]: string };
  /** The enforced maximum object size in bytes. */
  max_bytes: number;
  /** The policy's lifetime in seconds. */
  expires: number;
}

/** Options for {@link S3.usage}. */
interface S3UsageOptions {
  /** Key prefix to total, e.g. `"user-a/"`. Omit to total the whole bucket. */
  prefix?: string;
}

/** Result of {@link S3.usage}. */
interface S3UsageResult {
  /** The prefix that was totalled (empty string = whole bucket). */
  prefix: string;
  /** Total size in bytes of all objects under the prefix. */
  bytes: number;
  /** Number of objects under the prefix. */
  objects: number;
}

/** Options for {@link S3.delete}. */
interface S3DeleteOptions {
  /** Object key to delete, e.g. `"user-a/photo.jpg"`. */
  key: string;
}

/** Result of {@link S3.delete}. */
interface S3DeleteResult {
  /** The key that was deleted. */
  key: string;
  /** Always `true` on success (S3 delete is idempotent — a missing key still succeeds). */
  deleted: boolean;
}

/**
 * S3-compatible storage helper for `config.s3` (AWS S3, Cloudflare R2, MinIO,
 * Backblaze B2, …). Signing a URL is pure crypto — the server never touches your
 * files; `usage` and `delete` are the calls that connect. The `endpoint` is
 * operator-config and SSRF-guarded. The sign helpers / `delete` throw on an empty `key`.
 */
interface S3 {
  /** Signs a `PUT` upload link. */
  upload_url(opts: S3PresignOptions): S3PresignResult;
  /** Signs a `GET` download link. */
  download_url(opts: S3PresignOptions): S3PresignResult;
  /** Signs a size-limited browser POST upload form (cap from `config.s3.max_upload_size`). */
  upload_form(opts: S3PresignPostOptions): S3PresignPostResult;
  /** Signs a URL for any `method` (default `"PUT"`). `DELETE` needs `config.s3.allow_delete`. */
  sign_url(opts: S3PresignGeneralOptions): S3PresignResult;
  /**
   * Totals the bytes and object count under a key prefix by listing the bucket.
   * No native "folder size" exists in S3, so this walks every object under the
   * prefix; each 1000-object page counts against `max_ops`.
   * @example const u = s3.usage({ prefix: "user-a/" }); // { prefix, bytes, objects }
   */
  usage(opts?: S3UsageOptions): S3UsageResult;
  /**
   * Deletes one object. **Destructive and opt-in** — throws unless the operator
   * set `config.s3.allow_delete = true`, even when `s3` is otherwise configured.
   * @example const d = s3.delete({ key: "user-a/old.jpg" }); // { key, deleted: true }
   */
  delete(opts: S3DeleteOptions): S3DeleteResult;
}

/** S3 storage helper. Present only when `config.s3` is supplied. */
declare const s3: S3;

// ─────────────────────────────────────────────────────────────────────────────
// `redis` — key/value store (present when `config.redis` is set)
// ─────────────────────────────────────────────────────────────────────────────

/** Options for {@link Redis.set}. */
interface RedisSetOptions {
  /** Time-to-live in seconds (optional). */
  ttl?: number;
}

/**
 * Redis key/value helper for `config.redis` (trusted, operator-supplied — no SSRF
 * guard). **Strings in / strings out**: serialize objects yourself (`JSON.stringify`).
 * All calls are synchronous (no `await`), like `db`. A failure to reach Redis throws a
 * retryable `REDIS_CONNECTION` capability error.
 */
interface Redis {
  /** `GET key` — the string value, or `null` if the key is missing. */
  get(key: string): string | null;
  /** `SET key value [EX ttl]` — returns `true`. */
  set(key: string, value: string, opts?: RedisSetOptions): boolean;
  /** `DEL key` — number of keys removed (0 or 1). */
  del(key: string): number;
  /** `INCR key` — the new value. */
  incr(key: string): number;
  /** `EXPIRE key seconds` — `true` if the key existed and the TTL was set. */
  expire(key: string, seconds: number): boolean;
}

/** Redis helper. Present only when `config.redis` is supplied. */
declare const redis: Redis;

// ─────────────────────────────────────────────────────────────────────────────
// `amq` — RabbitMQ producer (present when `config.amq` is set)
// ─────────────────────────────────────────────────────────────────────────────

/** A single message: `[routingKey, payload]`. The payload is published as its JSON bytes. */
type AmqMessage = [routingKey: string, payload: unknown];

/**
 * RabbitMQ **producer** for `config.amq` (trusted, operator-supplied — no SSRF guard).
 * Producer only (no consume). Synchronous; the whole batch is one op against `max_ops`.
 * A broker outage throws a retryable `AMQ_CONNECTION` capability error; a batch larger
 * than `config.amq.max_batch` throws `AMQ_BATCH_TOO_LARGE`.
 */
interface Amq {
  /**
   * Publishes a batch and returns the number published. `routingKey` is the queue name
   * for the default exchange (override via `config.amq.exchange`).
   * @example amq.send([["user.created", { id: 1 }], ["user.created", { id: 2 }]]); // → 2
   */
  send(messages: AmqMessage[]): number;
}

/** RabbitMQ producer. Present only when `config.amq` is supplied. */
declare const amq: Amq;

// ─────────────────────────────────────────────────────────────────────────────
// `auth` — OIDC/IAM identity (present when `config.auth` is set)
// ─────────────────────────────────────────────────────────────────────────────

/** A valid token's resolved identity claims (`sub`, `email`, `name`, …). */
type AuthClaims = Record<string, unknown>;

/** {@link Auth.user_info} result — a discriminated union on `ok`. */
type AuthUserInfo =
  | { ok: true; claims: AuthClaims }
  | { ok: false; status: number; code: "AUTH_INVALID_TOKEN" };

/** {@link Auth.introspect} result (RFC 7662). Read `claims.active` to gate. */
interface AuthIntrospection {
  /** Always `true` on a successful round-trip (the IAM answered). */
  ok: true;
  /** The introspection response; `claims.active` tells you if the token is live. */
  claims: AuthClaims & { active: boolean };
}

/**
 * OIDC/IAM identity over an **operator-supplied** issuer (`config.auth`), so it is
 * trusted — no SSRF guard. Validation is delegated to the IAM (a `userinfo`
 * round-trip), so there is no local JWT/JWKS crypto. Endpoints are auto-discovered
 * from `{issuer}/.well-known/openid-configuration` unless overridden in config.
 *
 * **Hybrid errors:** a token-validity outcome is the caller's business flow, so an
 * invalid/expired/insufficient-scope token comes back **in-band** (`{ ok: false }`)
 * and never throws. Infra failures the handler can't act on (issuer down, misconfig)
 * **throw** a tagged capability error (`AUTH_UNAVAILABLE` / `AUTH_REQUEST`).
 */
interface Auth {
  /**
   * Resolves a bearer token's claims via the IAM userinfo endpoint.
   * @example
   * const u = auth.user_info(ctx.token);
   * if (!u.ok) return json(null, { code: "unauthorized" });
   * return json({ id: u.claims.sub }, null);
   */
  user_info(token: string): AuthUserInfo;
  /**
   * RFC 7662 token introspection. Needs `config.auth.client_id` / `client_secret`.
   * @example const r = auth.introspect(ctx.token); if (!r.claims.active) { ... }
   */
  introspect(token: string): AuthIntrospection;
}

/** OIDC/IAM identity helper. Present only when `config.auth` is supplied. */
declare const auth: Auth;

// ─────────────────────────────────────────────────────────────────────────────
// `$sys` — runtime stdlib: crypto + date (always on); env/secrets when config.sys set
// ─────────────────────────────────────────────────────────────────────────────

/** HMAC hash algorithm. */
type SysHmacAlgo = "sha256" | "sha512";

/** Output encoding for an HMAC digest. */
type SysEncoding = "hex" | "base64" | "base64url";

/** A reversible encode/decode pair (UTF-8 string ⇄ encoded string). */
interface SysCodec {
  /** Encodes a UTF-8 string. */
  encode(input: string): string;
  /** Decodes back to a UTF-8 string (throws on invalid input). */
  decode(input: string): string;
}

/**
 * Pure crypto + encoding helpers (always available). Hashing/HMAC are one-way;
 * the codecs are reversible. A {@link SysSecret} handle may be passed **only** as the
 * `key` of {@link hmac} — every other helper takes a plain `string` and so rejects it.
 */
interface SysCrypto {
  /** SHA-256 of `data`, hex-encoded. */
  sha256(data: string): string;
  /** SHA-512 of `data`, hex-encoded. */
  sha512(data: string): string;
  /**
   * HMAC of `msg` under `key`, `encoding`-encoded (default `"hex"`). `key` may be a
   * {@link SysSecret} handle — it is resolved server-side; the plaintext never enters JS.
   * @example $sys.crypto.hmac("sha256", $sys.secrets.SIGNING_KEY, body);
   */
  hmac(
    algo: SysHmacAlgo,
    key: string | SysSecret,
    msg: string,
    encoding?: SysEncoding,
  ): string;
  /** A random v4 UUID. */
  uuid(): string;
  /** Standard base64 codec. */
  base64: SysCodec;
  /** URL-safe base64 (no padding) codec. */
  base64url: SysCodec;
  /** Hex codec. */
  hex: SysCodec;
  /** Percent-encoding (URL escape) codec. */
  url: SysCodec;
}

/**
 * A fixed-length duration for {@link SysDate.add} / {@link SysDate.sub}, like Python's
 * `timedelta`. Only constant-length units — no months/years (ambiguous length).
 */
interface SysDuration {
  weeks?: number;
  days?: number;
  hours?: number;
  minutes?: number;
  seconds?: number;
  ms?: number;
}

/** The gap between two dates, from {@link SysDate.diff}. */
interface SysDateDiff {
  /** Signed total milliseconds (`this - other`). */
  total_ms: number;
  /** Signed total seconds. */
  total_seconds: number;
  /** Whole days in the absolute gap. */
  days: number;
  /** Remaining whole hours (0–23). */
  hours: number;
  /** Remaining whole minutes (0–59). */
  minutes: number;
  /** Remaining whole seconds (0–59). */
  seconds: number;
}

/**
 * An immutable UTC instant. Arithmetic is method-based and returns a new instance;
 * serializes as its RFC 3339 string inside {@link json} / `JSON.stringify`.
 */
interface SysDate {
  /** A new instant shifted forward by `delta`. */
  add(delta: SysDuration): SysDate;
  /** A new instant shifted backward by `delta`. */
  sub(delta: SysDuration): SysDate;
  /** Breakdown of `this - other` (accepts another instant or epoch millis). */
  diff(other: SysDate | number): SysDateDiff;
  /** RFC 3339 string in UTC, e.g. `"2026-06-08T00:00:00Z"`. */
  iso(): string;
  /** Epoch seconds. */
  unix(): number;
  /** Epoch milliseconds (the canonical value). */
  epochMs(): number;
  /** Serializes as {@link iso}. */
  toJSON(): string;
  /** Serializes as {@link iso}. */
  toString(): string;
}

/** Date helpers (always available). Parsing normalizes everything to UTC. */
interface SysDateFactory {
  /** The current instant (UTC). */
  now(): SysDate;
  /**
   * Parses an ISO 8601 / RFC 3339 string (offset-aware), a `YYYY-MM-DD` date, or epoch
   * millis → a UTC {@link SysDate}. Throws on unparseable input.
   * @example $sys.date.parse(ctx.when).add({ days: 3 }).iso();
   */
  parse(input: string | number | SysDate): SysDate;
}

/**
 * An opaque secret handle from {@link Sys.secrets}. The plaintext **never enters JS** —
 * pass it as the `key` of {@link SysCrypto.hmac}; any coercion (`String(x)`, a template
 * literal, `JSON.stringify`) yields `"[secret:NAME]"`, never the value.
 */
interface SysSecret {
  /** Yields `"[secret:NAME]"` — never the plaintext. */
  toString(): string;
  /** Yields `"[secret:NAME]"` — never the plaintext. */
  toJSON(): string;
}

/**
 * The `$sys` runtime standard library. `crypto` and `date` are pure and **always**
 * available; `env` and `secrets` are populated only when `config.sys` is supplied
 * (otherwise they are empty objects).
 */
interface Sys {
  /** Pure crypto + encoding (always available). */
  crypto: SysCrypto;
  /** Date parse + timedelta math (always available). */
  date: SysDateFactory;
  /**
   * Plain, returnable operator config values from `config.sys.env`. Typed as possibly
   * `undefined` so you can probe optional keys (`$sys.env.FLAG === undefined`); a key
   * you know is set can be used directly.
   */
  env: { readonly [key: string]: string | undefined };
  /**
   * Opaque secret handles from `config.sys.secrets` (see {@link SysSecret}). Typed as
   * always-present (you reference the keys you provisioned) so a handle drops straight
   * into {@link SysCrypto.hmac} without a null check; an unprovisioned key is `undefined`
   * at runtime.
   */
  secrets: { readonly [key: string]: SysSecret };
}

/** Runtime stdlib. `$sys.crypto` / `$sys.date` always available; `env` / `secrets` need `config.sys`. */
declare const $sys: Sys;
