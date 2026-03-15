globalThis.json = function(data, errors) {
  return JSON.stringify({
    data: data !== undefined ? data : null,
    errors: errors !== undefined ? errors : null
  });
};