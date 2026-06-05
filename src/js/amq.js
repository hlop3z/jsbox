(function() {
  function call(action, payload) {
    var raw = __amq(action, JSON.stringify(payload || {}));
    var res = JSON.parse(raw);
    if (res && res.error) {
      var err = new Error(res.error);
      err.__jsbox = res; // { error, code, retryable, owner, source } — engine classifies off this
      throw err;
    }
    return res;
  }
  globalThis.amq = {
    // Publish a batch: amq.send([[routingKey, payload], ...]). Rust owns batching.
    // Returns the number published. Synchronous (no await).
    send: function(list) {
      list = list || [];
      // Ergonomic: normalize a single ["key", payload] into [["key", payload]].
      if (list.length === 2 && typeof list[0] === 'string') list = [list];
      var messages = [];
      for (var i = 0; i < list.length; i++) {
        messages.push({ key: list[i][0], payload: list[i][1] });
      }
      return call('send', { messages: messages }).published;
    }
  };
})();
