(function() {
  function call(action, payload) {
    var raw = __redis(action, JSON.stringify(payload || {}));
    var res = JSON.parse(raw);
    if (res && res.error) {
      var err = new Error(res.error);
      err.__jsbox = res; // { error, code, retryable, owner, source } — engine classifies off this
      throw err;
    }
    return res;
  }
  globalThis.redis = {
    // strings in/out; get of a missing key returns null. Synchronous (no await).
    get: function(key) { return call('get', { key: key }).value; },
    set: function(key, value, opts) {
      opts = opts || {};
      // ttl is optional, in seconds. Value is coerced to a string (the script owns JSON).
      return call('set', { key: key, value: String(value), ttl: opts.ttl }).ok;
    },
    del: function(key) { return call('del', { key: key }).count; },
    incr: function(key) { return call('incr', { key: key }).value; },
    expire: function(key, seconds) { return call('expire', { key: key, seconds: seconds }).set; }
  };
})();
