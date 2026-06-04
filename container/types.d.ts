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
 * `json`, `$`, and `Decimal` are pure and **always** available.
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
  /** HTTP status code, or `0` if the request was blocked or failed. */
  status: number;
  /** Parsed JSON response body; the raw string if it wasn't valid JSON. */
  data: T;
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
  get<T = any>(url: string, params?: QueryParams, headers?: HttpHeaders): ApiResponse<T>;
  /** `POST url` with a JSON `body`. */
  post<T = any>(url: string, body?: unknown, headers?: HttpHeaders): ApiResponse<T>;
  /** `PUT url` with a JSON `body`. */
  put<T = any>(url: string, body?: unknown, headers?: HttpHeaders): ApiResponse<T>;
  /** `PATCH url` with a JSON `body`. */
  patch<T = any>(url: string, body?: unknown, headers?: HttpHeaders): ApiResponse<T>;
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

/** Options for {@link S3.presignPut} / {@link S3.presignGet}. */
interface S3PresignOptions {
  /** Object key (path within the bucket), e.g. `"uploads/photo.jpg"`. */
  key: string;
  /** Link lifetime in seconds. Defaults to `config.s3.expires`; capped at `max_expires`. */
  expires?: number;
}

/** Options for the general {@link S3.presign}. */
interface S3PresignGeneralOptions extends S3PresignOptions {
  /** HTTP method to sign for. Defaults to `"PUT"`. */
  method?: S3Method;
}

/** Result of {@link S3.presign} / {@link S3.presignPut} / {@link S3.presignGet}. */
interface S3PresignResult {
  /** The signed URL the browser uses directly. */
  url: string;
  /** The method the URL is signed for. */
  method: S3Method;
  /** The link's lifetime in seconds. */
  expires: number;
}

/** Options for {@link S3.presignPost}. */
interface S3PresignPostOptions {
  /** Object key the upload will be stored under. */
  key: string;
  /** Link lifetime in seconds. Defaults to `config.s3.expires`. */
  expires?: number;
}

/**
 * Result of {@link S3.presignPost} — a browser POST policy whose size limit the
 * object store enforces (the cap comes from `config.s3.max_upload_size`).
 */
interface S3PresignPostResult {
  /** The POST target URL. */
  url: string;
  /** Form fields to send before the `file` part. */
  fields: { [field: string]: string };
  /** The enforced maximum object size in bytes. */
  maxBytes: number;
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
 * Backblaze B2, …). `presign*` is pure crypto — the server never touches your
 * files; `usage` and `delete` are the calls that connect. The `endpoint` is
 * operator-config and SSRF-guarded. `presign*`/`delete` throw on an empty `key`.
 */
interface S3 {
  /** Signs a URL for the given `method` (default `"PUT"`). `DELETE` needs `config.s3.allow_delete`. */
  presign(opts: S3PresignGeneralOptions): S3PresignResult;
  /** Signs a `PUT` upload URL. */
  presignPut(opts: S3PresignOptions): S3PresignResult;
  /** Signs a `GET` download URL. */
  presignGet(opts: S3PresignOptions): S3PresignResult;
  /** Signs a size-limited browser POST upload (cap from `config.s3.max_upload_size`). */
  presignPost(opts: S3PresignPostOptions): S3PresignPostResult;
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
