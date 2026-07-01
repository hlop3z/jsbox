(function() {
  // Generic egress wrapper. Per-capability wrappers (db.js, mail.js, …) will call into the
  // same `__io` FFI underneath; this `io.call` is the explicit, low-level surface.
  // Mirrors db.js: a successful call returns parsed JSON; a failed call returns a `__runlet`
  // tagged object the wrapper re-throws so the engine classifies it as a capability error.
  function call(name, action, payload) {
    var raw = __io(name, action, JSON.stringify(payload === undefined ? null : payload));
    var res = JSON.parse(raw);
    if (res && res.error) {
      var err = new Error(res.error);
      err.__runlet = res; // { error, code, retryable, owner, source, details? }
      throw err;
    }
    return res;
  }
  globalThis.io = { call: call };
})();
