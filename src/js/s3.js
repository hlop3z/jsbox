(function() {
  function call(action, payload) {
    var raw = __s3(action, JSON.stringify(payload || {}));
    var res = JSON.parse(raw);
    if (res && res.error) throw new Error(res.error);
    return res;
  }
  function presign(opts) {
    opts = opts || {};
    return call('presign', {
      method: opts.method || 'PUT',
      key: opts.key || '',
      expires: opts.expires || 0
    });
  }
  globalThis.s3 = {
    presign: presign,
    presignPut: function(opts) {
      opts = opts || {};
      return presign({ method: 'PUT', key: opts.key, expires: opts.expires });
    },
    presignGet: function(opts) {
      opts = opts || {};
      return presign({ method: 'GET', key: opts.key, expires: opts.expires });
    }
  };
})();
