function handler(ctx) {
  // Size-enforced upload link. The max size is NOT here -- it comes from
  // config.s3.max_upload_size (secrets.json). The browser POSTs the file
  // straight to the bucket; the store rejects anything over the cap.
  const up = s3.upload_form({ key: ctx.key, expires: 300 });
  // up = { url, fields: { ...form fields... }, max_bytes, expires }

  // A short-lived signed download link for the same object.
  const down = s3.download_url({ key: ctx.key, expires: 300 });

  return json({ upload: up, download: down.url }, null);
}
