(function () {
  // Profile::Deterministic enforcement: neutralize nondeterministic surfaces so the same
  // (code, context, declared-dependency reads) always produce the same result + effects.
  // Runs AFTER eval/Proxy removal. Overrides throw rather than return a fixed value, so a
  // handler that depends on wall-clock/randomness fails loudly instead of silently.
  function disabled(name) {
    return function () {
      throw new Error(name + " is disabled in the deterministic profile");
    };
  }

  if (typeof Math !== "undefined") {
    Math.random = disabled("Math.random");
  }

  if (typeof Date !== "undefined") {
    var RealDate = Date;
    // Block reading the wall clock via `new Date()` / `Date()` (no args) while keeping
    // explicit construction (`new Date(ms)`, `new Date(y, m, ...)`) and the pure statics.
    var SafeDate = function (a, b, c, d, e, f, g) {
      if (arguments.length === 0) {
        throw new Error(
          "new Date() (current time) is disabled in the deterministic profile"
        );
      }
      switch (arguments.length) {
        case 1: return new RealDate(a);
        case 2: return new RealDate(a, b);
        case 3: return new RealDate(a, b, c);
        case 4: return new RealDate(a, b, c, d);
        case 5: return new RealDate(a, b, c, d, e);
        case 6: return new RealDate(a, b, c, d, e, f);
        default: return new RealDate(a, b, c, d, e, f, g);
      }
    };
    SafeDate.prototype = RealDate.prototype;
    SafeDate.parse = RealDate.parse;
    SafeDate.UTC = RealDate.UTC;
    SafeDate.now = disabled("Date.now");
    globalThis.Date = SafeDate;
  }

  if (typeof $sys !== "undefined") {
    if ($sys.date) $sys.date.now = disabled("$sys.date.now");
    if ($sys.crypto) $sys.crypto.uuid = disabled("$sys.crypto.uuid");
  }
})();
