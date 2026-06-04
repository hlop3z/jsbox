function handler(ctx) {
  // A non-2xx HTTP response is data (res.status), not a thrown error. A transport
  // failure comes back in-band as res.error. Happy path: echo status + a field.
  const res = api.get(ctx.url);
  // res = { status, data, ... }
  return json({ status: res.status, title: res.data.title }, null);
}
