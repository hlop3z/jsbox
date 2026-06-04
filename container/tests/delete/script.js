function handler(ctx) {
  // Destructive op -- only works because secrets.json sets config.s3.allow_delete = true.
  const del = s3.delete({ key: ctx.key });
  // Prove it's gone: usage of the prefix should now be empty (0 bytes / 0 objects).
  const after = s3.usage({ prefix: ctx.prefix });
  return json({ deleted: del, after: after }, null);
}
