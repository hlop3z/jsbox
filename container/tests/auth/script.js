function handler(ctx) {
  // auth validates a caller's bearer token against your IAM (operator config in
  // config.auth; trusted, no SSRF guard). The userinfo endpoint is auto-discovered
  // from config.auth.issuer via {issuer}/.well-known/openid-configuration.
  const u = auth.user_info(ctx.token);
  // u = { ok: true, claims: { sub, ... } }
  //   | { ok: false, status, code: "AUTH_INVALID_TOKEN" }   (in-band, never throws)
  if (!u.ok) {
    return json(null, { code: "unauthorized", status: u.status });
  }

  // Optional RFC 7662 introspection (needs config.auth.client_id/secret) tells you
  // whether the token is still live (revocation/expiry):
  //   const r = auth.introspect(ctx.token);
  //   if (!r.claims.active) return json(null, { code: "token_revoked" });

  return json({ sub: u.claims.sub, email: u.claims.email || null }, null);
}
