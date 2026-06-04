function handler(ctx) {
  const req = {
    to: ctx.email, // one address, or a list: ["a@x.com", "b@x.com"]
    subject: "Welcome, " + ctx.name + "!",
    html: "<b>Thanks for joining us.</b>", // optional, makes it pretty
  };
  const res = mail.send(req);
  // res = { accepted: true, response: "2.0.0 Ok: queued ..." }
  return json(res, null);
}
