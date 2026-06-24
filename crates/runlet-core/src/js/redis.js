(function() {
  // Routes through the generic resource egress (throws a tagged capability error on failure).
  function call(action, payload) {
    return resource.call('redis', action, payload || {});
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
