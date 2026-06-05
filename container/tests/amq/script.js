function handler(ctx) {
  // amq is a RabbitMQ producer (trusted, operator config; no SSRF guard). List-always:
  // [[routingKey, payload], ...]. The whole batch is one op; Rust opens one connection.
  // routingKey is the queue name for the default exchange; payload is published as JSON.
  const published = amq.send([
    [ctx.queue, { event: "created", id: 1 }],
    [ctx.queue, { event: "created", id: 2 }],
  ]);
  return json({ published: published }, null); // -> { published: 2 }
}
