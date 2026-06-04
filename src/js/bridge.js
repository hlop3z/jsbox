globalThis.json = function(data, error) {
  return JSON.stringify({
    data: data !== undefined ? data : null,
    error: error !== undefined ? error : null
  });
};