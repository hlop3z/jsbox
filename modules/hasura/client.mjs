// Thin GraphQL client for Hasura, layered on jsbox's SSRF-guarded `api` capability.
//
// It absorbs the three things every Hasura-from-jsbox handler otherwise repeats:
//   1. building the `/v1/graphql` URL,
//   2. attaching auth headers (admin secret, or a forwarded end-user JWT + role),
//   3. surfacing the GraphQL errors Hasura returns *inside an HTTP 200* body — the
//      part naive `api` code misses because it only checks `res.status`.
//
// Operator config (kept out of the handler — see docs/09-sys.md and docs/02-api.md):
//   - config.api.allowed_hosts          must include the Hasura host (api is SSRF-guarded).
//   - config.sys.env.HASURA_ENDPOINT    e.g. "https://hasura.internal"
//   - config.sys.env.HASURA_ADMIN_SECRET  (optional; omit when forwarding a user JWT)
//   - config.modules_dir                must point at the folder holding this file, so
//                                       `import { hasura } from "hasura/client"` resolves.
//
// Safe path only: callers pass `variables`; values are never string-interpolated into the
// query (Hasura's parameterization — the GraphQL analogue of db's $1,$2 placeholders).

/**
 * Create a Hasura client bound to a set of credentials / a role.
 *
 * @param {object} [opts]
 * @param {string} [opts.endpoint]     Base URL; defaults to `$sys.env.HASURA_ENDPOINT`.
 * @param {string} [opts.token]        End-user JWT, sent as `Authorization: Bearer` so
 *                                     Hasura enforces that user's row-level permissions.
 * @param {string} [opts.adminSecret]  Admin secret; defaults to `$sys.env.HASURA_ADMIN_SECRET`.
 *                                     Ignored when `token` is set (the JWT wins).
 * @param {string} [opts.role]         Sent as `x-hasura-role` to select a permission role.
 * @param {object} [opts.headers]      Extra headers merged onto every request.
 */
export function hasura(opts = {}) {
  const env = typeof $sys !== "undefined" ? $sys.env : {};
  const endpoint = opts.endpoint || env.HASURA_ENDPOINT;
  if (!endpoint) {
    throw new Error(
      "hasura: no endpoint — set config.sys.env.HASURA_ENDPOINT or pass { endpoint }",
    );
  }
  const url = endpoint.replace(/\/+$/, "") + "/v1/graphql";

  function headers() {
    const h = { "content-type": "application/json" };
    if (opts.token) {
      h["authorization"] = "Bearer " + opts.token; // user JWT → row-level perms
    } else {
      const secret = opts.adminSecret || env.HASURA_ADMIN_SECRET;
      if (secret) h["x-hasura-admin-secret"] = secret; // backend role → bypasses perms
    }
    if (opts.role) h["x-hasura-role"] = opts.role;
    return Object.assign(h, opts.headers || {});
  }

  // Run an operation and return Hasura's raw envelope ({ data?, errors? }). Never throws
  // on a GraphQL-level error — inspect `.errors` yourself. A transport failure (api's
  // in-band `status: 0`) is normalized into the same `errors` shape so one check covers both.
  function raw(query, variables) {
    const res = api.post(url, { query, variables: variables || {} }, headers());
    if (res.status === 0) {
      const code = (res.error && res.error.code) || "HASURA_TRANSPORT";
      return {
        errors: [
          {
            message: "hasura transport error",
            extensions: { code, transport: res.error },
          },
        ],
      };
    }
    return res.data || {};
  }

  // Run an operation and return ONLY `data`. Throws on a GraphQL or transport error; the
  // thrown Error carries `.graphql` (the errors array) and `.code` (the first error's code).
  function run(query, variables) {
    const body = raw(query, variables);
    if (body.errors && body.errors.length) {
      const first = body.errors[0];
      const err = new Error(first.message || "hasura error");
      err.graphql = body.errors;
      err.code = (first.extensions && first.extensions.code) || "HASURA_ERROR";
      throw err;
    }
    return body.data;
  }

  // query() and mutate() are the same wire call (both POST /v1/graphql) — two names so
  // call sites read right. raw() is the in-band escape hatch for inspecting errors inline.
  return { query: run, mutate: run, raw };
}

export default hasura;
