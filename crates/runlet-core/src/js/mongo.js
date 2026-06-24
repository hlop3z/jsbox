(function() {
  // Routes through the generic io egress; the collection is packed into the payload
  // ({collection, data}) since io.call carries a single payload. io.call throws a
  // tagged capability error on failure (see js/io.js).
  function call(action, collection, payload) {
    return io.call('mongo', action, { collection: collection, data: payload || {} });
  }
  globalThis.mongo = {
    // Reads. Values that don't fit a JS number exactly come back as strings
    // (Int64/Decimal128), as do ObjectId (hex), Date (RFC3339), and Binary (base64).
    find: function(collection, filter, options) {
      return call('find', collection, { filter: filter || {}, options: options || {} });
    },
    find_one: function(collection, filter) {
      return call('find_one', collection, { filter: filter || {} });
    },
    count: function(collection, filter) {
      return call('count', collection, { filter: filter || {} }).count;
    },
    aggregate: function(collection, pipeline) {
      return call('aggregate', collection, { pipeline: pipeline || [] });
    },
    // Writes.
    insert_one: function(collection, doc) {
      return call('insert_one', collection, { doc: doc || {} });
    },
    insert_many: function(collection, docs) {
      return call('insert_many', collection, { docs: docs || [] });
    },
    update_one: function(collection, filter, update) {
      return call('update_one', collection, { filter: filter || {}, update: update || {} });
    },
    update_many: function(collection, filter, update) {
      return call('update_many', collection, { filter: filter || {}, update: update || {} });
    },
    delete_one: function(collection, filter) {
      return call('delete_one', collection, { filter: filter || {} });
    },
    delete_many: function(collection, filter) {
      return call('delete_many', collection, { filter: filter || {} });
    }
  };
})();
