function handler(ctx) {
  // Core path: pure JS + the always-on `$` (Decimal) global. No capability
  // config needed, so this exercises the plain success envelope: { data, error: null }.
  const total = $(ctx.price).mul(ctx.qty).round(2);
  return json({ greeting: "hello " + ctx.name, total: total.toString() }, null);
}
