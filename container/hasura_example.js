// Example: query Hasura with the `hasura/client` injectable module.
//
// Module-mode handler (note the top-level `import` + `export default`). Deploy
// `modules/hasura/client.mjs` under config.modules_dir, add the Hasura host to
// config.api.allowed_hosts, and set config.sys.env.HASURA_ENDPOINT (+ either a
// forwarded ctx.token or config.sys.env.HASURA_ADMIN_SECRET).

import { hasura } from "hasura/client";

const USER_BY_ID = `
  query ($id: uuid!) {
    users_by_pk(id: $id) { id email name }
  }
`;

/** @type {Handler} */
export default function handler(ctx) {
  // Forward the caller's JWT so Hasura enforces that user's row-level permissions.
  // Drop `token` to fall back to the admin secret (backend/trusted jobs only).
  const h = hasura({ token: ctx.token });

  try {
    const data = h.query(USER_BY_ID, { id: ctx.userId });
    return json(data.users_by_pk, null);
  } catch (e) {
    // GraphQL/transport errors throw; `.code` + `.graphql` carry the detail.
    const err = /** @type {{ code?: string, message?: string, graphql?: unknown }} */ (e);
    return json(null, { code: err.code, message: err.message, graphql: err.graphql });
  }
}
