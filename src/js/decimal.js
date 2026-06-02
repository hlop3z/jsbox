(function () {
  function call(op, a, b) {
    var raw = __decimal(op, a, b === undefined ? "" : b);
    var res = JSON.parse(raw);
    if (res && res.error) throw new Error(res.error);
    return res.v;
  }

  function Dec(value) {
    this.v = value;
  }

  // Turn anything (number, string, or another Dec) into a decimal string.
  function coerce(x) {
    if (x instanceof Dec) return x.v;
    if (x === undefined || x === null) return "0";
    return String(x);
  }

  Dec.prototype.add = function (o) { return new Dec(call("add", this.v, coerce(o))); };
  Dec.prototype.sub = function (o) { return new Dec(call("sub", this.v, coerce(o))); };
  Dec.prototype.mul = function (o) { return new Dec(call("mul", this.v, coerce(o))); };
  Dec.prototype.div = function (o) { return new Dec(call("div", this.v, coerce(o))); };
  Dec.prototype.neg = function () { return new Dec(call("neg", this.v)); };
  Dec.prototype.abs = function () { return new Dec(call("abs", this.v)); };
  Dec.prototype.round = function (places) {
    return new Dec(call("round", this.v, String(places === undefined ? 0 : places)));
  };
  Dec.prototype.cmp = function (o) { return parseInt(call("cmp", this.v, coerce(o)), 10); };
  Dec.prototype.eq = function (o) { return this.cmp(o) === 0; };
  Dec.prototype.lt = function (o) { return this.cmp(o) < 0; };
  Dec.prototype.lte = function (o) { return this.cmp(o) <= 0; };
  Dec.prototype.gt = function (o) { return this.cmp(o) > 0; };
  Dec.prototype.gte = function (o) { return this.cmp(o) >= 0; };
  Dec.prototype.isZero = function () { return this.cmp(0) === 0; };
  Dec.prototype.isNegative = function () { return this.cmp(0) < 0; };
  Dec.prototype.toString = function () { return this.v; };
  Dec.prototype.toNumber = function () { return Number(this.v); };
  // Lets json()/JSON.stringify serialize a decimal as its exact string value.
  Dec.prototype.toJSON = function () { return this.v; };

  function make(value) {
    if (value instanceof Dec) return value;
    return new Dec(call("parse", coerce(value)));
  }

  globalThis.$ = make;
  globalThis.Decimal = make;
})();
