(function() {
  // Routes every db operation through the generic `resource.call("db", …)` egress (which packs
  // {sql, params} into one payload, dispatches to the wired Resource backend, and throws a
  // tagged capability error on failure — see js/resource.js). No direct native call here.
  function call(action, sql, params) {
    return resource.call('db', action, { sql: sql, params: params || [] });
  }
  globalThis.db = {
    query: function(sql, params) { return call('query', sql, params); },
    execute: function(sql, params) { return call('execute', sql, params); },
    begin: function() { call('begin', '', []); },
    commit: function() { call('commit', '', []); },
    rollback: function() { call('rollback', '', []); }
  };
})();
