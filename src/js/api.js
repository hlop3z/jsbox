(function() {
  function buildUrl(url, params) {
    if (!params) return url;
    var qs = [];
    var keys = Object.keys(params);
    for (var i = 0; i < keys.length; i++) {
      qs.push(encodeURIComponent(keys[i]) + '=' + encodeURIComponent(String(params[keys[i]])));
    }
    if (qs.length === 0) return url;
    return url + (url.indexOf('?') >= 0 ? '&' : '?') + qs.join('&');
  }
  function parse(raw) {
    try { return JSON.parse(raw); } catch(e) { return raw; }
  }
  function headersJson(h) {
    return (h && typeof h === 'object') ? JSON.stringify(h) : '';
  }
  function req(method, url, body, headers) {
    var bodyStr = (body !== undefined && body !== null) ? JSON.stringify(body) : '';
    var raw = __http(method, url, bodyStr, headersJson(headers));
    var res = JSON.parse(raw);
    res.data = parse(res.body);
    delete res.body;
    return res;
  }
  globalThis.api = {
    get: function(url, params, headers) { return req('GET', buildUrl(url, params), null, headers); },
    post: function(url, body, headers) { return req('POST', url, body, headers); },
    put: function(url, body, headers) { return req('PUT', url, body, headers); },
    patch: function(url, body, headers) { return req('PATCH', url, body, headers); },
    delete: function(url, headers) { return req('DELETE', url, null, headers); }
  };
})();