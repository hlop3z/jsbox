(function() {
  function call(action, payload) {
    var raw = __s3(action, JSON.stringify(payload || {}));
    var res = JSON.parse(raw);
    if (res && res.error) {
      var err = new Error(res.error);
      err.__runlet = res; // { error, code, retryable, owner, source } — engine classifies off this
      throw err;
    }
    return res;
  }
  function sign(opts) {
    opts = opts || {};
    return call('presign', {
      method: opts.method || 'PUT',
      key: opts.key || '',
      expires: opts.expires || 0
    });
  }
  globalThis.s3 = {
    // Sign a URL for any method (PUT/GET/HEAD/DELETE). Use the helpers below for the
    // common cases.
    sign_url: sign,
    // Sign an upload (PUT) link.
    upload_url: function(opts) {
      opts = opts || {};
      return sign({ method: 'PUT', key: opts.key, expires: opts.expires });
    },
    // Sign a download (GET) link.
    download_url: function(opts) {
      opts = opts || {};
      return sign({ method: 'GET', key: opts.key, expires: opts.expires });
    },
    // Sign a size-limited browser POST upload form. No size field: the cap comes only
    // from config.s3.max_upload_size.
    upload_form: function(opts) {
      opts = opts || {};
      return call('presign_post', { key: opts.key || '', expires: opts.expires || 0 });
    },
    // Total { prefix, bytes, objects } for a key prefix (e.g. "user-a/").
    usage: function(opts) {
      opts = opts || {};
      return call('usage', { prefix: opts.prefix || '' });
    },
    // Delete one object -> { key, deleted: true }. Throws unless the operator set
    // config.s3.allow_delete = true (destructive, so it is opt-in).
    delete: function(opts) {
      opts = opts || {};
      return call('delete', { key: opts.key || '' });
    }
  };
})();
