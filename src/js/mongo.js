(function() {
  function call(action, collection, payload) {
    var raw = __mongo(action, collection, JSON.stringify(payload || {}));
    var res = JSON.parse(raw);
    if (res && res.error) {
      var err = new Error(res.error);
      err.__jsbox = res; // { error, code, retryable, owner, source } — engine classifies off this
      throw err;
    }
    return res;
  }
  globalThis.mongo = {
    // Reads. Values that don't fit a JS number exactly come back as strings
    // (Int64/Decimal128), as do ObjectId (hex), Date (RFC3339), and Binary (base64).
    find: function(collection, filter, options) {
      return call('find', collection, { filter: filter || {}, options: options || {} });
    },
    findOne: function(collection, filter) {
      return call('findOne', collection, { filter: filter || {} });
    },
    count: function(collection, filter) {
      return call('count', collection, { filter: filter || {} }).count;
    },
    aggregate: function(collection, pipeline) {
      return call('aggregate', collection, { pipeline: pipeline || [] });
    },
    // Writes.
    insertOne: function(collection, doc) {
      return call('insertOne', collection, { doc: doc || {} });
    },
    insertMany: function(collection, docs) {
      return call('insertMany', collection, { docs: docs || [] });
    },
    updateOne: function(collection, filter, update) {
      return call('updateOne', collection, { filter: filter || {}, update: update || {} });
    },
    updateMany: function(collection, filter, update) {
      return call('updateMany', collection, { filter: filter || {}, update: update || {} });
    },
    deleteOne: function(collection, filter) {
      return call('deleteOne', collection, { filter: filter || {} });
    },
    deleteMany: function(collection, filter) {
      return call('deleteMany', collection, { filter: filter || {} });
    }
  };
})();
