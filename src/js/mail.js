(function() {
  function norm(value) {
    if (value === undefined || value === null) return [];
    return Array.isArray(value) ? value : [value];
  }
  function call(action, payload) {
    var raw = __mail(action, JSON.stringify(payload || {}));
    var res = JSON.parse(raw);
    if (res && res.error) {
      var err = new Error(res.error);
      err.__jsbox = res; // { error, code, retryable, source } — engine classifies off this
      throw err;
    }
    return res;
  }
  globalThis.mail = {
    send: function(opts) {
      opts = opts || {};
      return call('send', {
        from: opts.from || '',
        to: norm(opts.to),
        cc: norm(opts.cc),
        bcc: norm(opts.bcc),
        reply_to: opts.reply_to || '',
        subject: opts.subject || '',
        text: opts.text || '',
        html: opts.html || ''
      });
    }
  };
})();
