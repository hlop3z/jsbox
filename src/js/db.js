(function() {
  function call(action, sql, params) {
    var raw = __db(action, sql, JSON.stringify(params || []));
    var res = JSON.parse(raw);
    if (res && res.error) throw new Error(res.error);
    return res;
  }
  globalThis.db = {
    query: function(sql, params) { return call('query', sql, params); },
    execute: function(sql, params) { return call('execute', sql, params); },
    begin: function() { call('begin', '', []); },
    commit: function() { call('commit', '', []); },
    rollback: function() { call('rollback', '', []); }
  };
})();