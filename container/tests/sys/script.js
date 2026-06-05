function handler(ctx) {
  // $sys is always on -- pure helpers, no config needed.

  // -- crypto: hashing, hmac, uuid, encoding --
  var hash = $sys.crypto.sha256("hello");
  var sig = $sys.crypto.hmac("sha256", "key", "msg"); // hex by default
  var sigB64 = $sys.crypto.hmac("sha256", "key", "msg", "base64");
  var id = $sys.crypto.uuid();
  var b64 = $sys.crypto.base64.encode("hi there");
  var roundtrip = $sys.crypto.base64.decode(b64);
  var hex = $sys.crypto.hex.encode("AB");
  var urlenc = $sys.crypto.url.encode("a b&c");

  // -- date: parse a frontend timestamp, timedelta math, diff --
  var when = $sys.date.parse(ctx.when); // ISO string from the frontend
  var due = when.add({ days: 3, hours: 12 }); // Python-style timedelta
  var back = due.sub({ weeks: 1 });
  var gap = due.diff(when); // { total_seconds, days, hours, ... }
  var fromEpoch = $sys.date.parse(1780000000000); // epoch millis also accepted

  return json(
    {
      hash: hash,
      sig: sig,
      sigB64: sigB64,
      uuidLen: id.length,
      b64: b64,
      roundtrip: roundtrip,
      hex: hex,
      urlenc: urlenc,
      when: when.iso(),
      due: due.iso(),
      back: back.iso(),
      dueUnix: due.unix(),
      gap: gap,
      fromEpoch: fromEpoch.iso(),
    },
    null
  );
}
