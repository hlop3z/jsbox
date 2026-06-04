function handler(ctx) {
  // db is trusted (operator-supplied connection, no SSRF guard). Basic round-trip:
  // a parameterized SELECT. db.query returns { rows, columns, row_count }.
  const r = db.query("SELECT $1::int AS answer, 'ok' AS status", [ctx.n]);
  return json({ answer: r.rows[0].answer, status: r.rows[0].status, rows: r.row_count }, null);
}
