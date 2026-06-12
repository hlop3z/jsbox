// Registry test fixture — key: `greet`
function handler(ctx) {
  return json("hello " + (ctx.name || "world"), null);
}
