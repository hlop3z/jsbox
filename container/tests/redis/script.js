function handler(ctx) {
  // redis is trusted (operator-supplied connection, no SSRF guard). Strings in/out --
  // the script owns (de)serialization. All calls are synchronous (no await).
  redis.set(ctx.key, ctx.value, { ttl: 60 }); // ttl seconds, optional
  const hits = redis.incr(ctx.key + ":hits"); // counter -> number
  const value = redis.get(ctx.key);           // string | null (null if missing)
  const ttlSet = redis.expire(ctx.key, 120);  // bool
  return json({ value: value, hits: hits, ttlSet: ttlSet }, null);
}
