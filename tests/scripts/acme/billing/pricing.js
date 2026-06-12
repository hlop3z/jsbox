// Registry test fixture — nested key: `acme/billing/pricing`
function handler(ctx) {
  return json({ total: ctx.qty * ctx.price }, null);
}
