// Test fixture module (injectable ES module). The SQL-builder-style "safe path only"
// helper from the design doc: a tiny pricing utility a handler can `import`.
export function quote(items, unit) {
  return items * unit;
}

export const TAX_RATE = 0.1;

export function withTax(amount) {
  return amount + amount * TAX_RATE;
}
