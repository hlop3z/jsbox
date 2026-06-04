function handler(ctx) {
  // Total bytes + object count under each "folder" (an S3 key prefix).
  // Seeded by ../../minio.sh:  user-a/ = 300 bytes / 2,  user-b/ = 50 bytes / 1.
  const a = s3.usage({ prefix: "user-a/" });
  const b = s3.usage({ prefix: "user-b/" });
  return json({ "user-a": a, "user-b": b }, null);
}
