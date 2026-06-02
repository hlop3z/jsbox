# 4. `mail` — Send Email ✉️

[← Back to the guide](README.md)

`mail` lets your script send a real email through a mail server (called an "SMTP relay").

## Turn it on first 🔑

Give the robot your mail server details with `config.mail`:

```json
{
  "config": {
    "mail": {
      "host": "smtp.example.com",
      "port": 587,
      "user": "apikey",
      "password": "secret",
      "tls": "starttls",
      "from": "no-reply@example.com"
    }
  }
}
```

| Setting          | What it means                                              | Default      |
| ---------------- | ---------------------------------------------------------- | ------------ |
| `host`           | Your mail server's address                                 | (must set)   |
| `port`           | The door number on the server                              | `587`        |
| `user`           | Login name (leave empty if your server needs none)         | `""`         |
| `password`       | Login password                                             | `""`         |
| `tls`            | How to keep it secret: `"starttls"`, `"wrapper"`, `"none"` | `"starttls"` |
| `from`           | The "from" address if your email doesn't set one           | (must set)   |
| `max_recipients` | Most people one email may go to                            | `50`         |
| `timeout_ms`     | How long to wait before giving up                          | `10000`      |

No `config.mail` → `mail` is turned off.

> **Which `tls`?** Most servers use `"starttls"` (port 587). Some older ones use
> `"wrapper"` (port 465). Use `"none"` only for local testing with no security.

## Send an email: `mail.send`

```js
function handler(ctx) {
  var res = mail.send({
    to: ctx.email, // one address, or a list: ["a@x.com", "b@x.com"]
    subject: "Welcome, " + ctx.name + "!",
    text: "Thanks for joining us.",
    html: "<b>Thanks for joining us.</b>", // optional, makes it pretty
  });
  // res = { accepted: true, response: "2.0.0 Ok: queued ..." }
  return json(res, null);
}
```

That's it! `accepted: true` means the mail server took your email. 🎉

## All the things you can set

```js
mail.send({
  from: "Team <hello@example.com>", // optional (else uses config.mail.from)
  to: ["alice@example.com", "bob@example.com"], // one or many
  cc: ["boss@example.com"], // optional
  bcc: ["secret@example.com"], // optional (hidden copy)
  reply_to: "support@example.com", // optional
  subject: "Hi there",
  text: "Plain words.", // optional
  html: "<h1>Fancy words.</h1>", // optional
});
```

- Give **`text`**, **`html`**, or **both**. If you give both, email apps that can
  show pretty emails use the `html`, and plain ones use the `text`.
- `to`, `cc`, and `bcc` each accept **one address** or **a list** of addresses.

## When something goes wrong

`mail.send` **throws an error** if it can't send (bad address, too many people, server
said no). Catch it with `try/catch`:

```js
function handler(ctx) {
  try {
    mail.send({ to: ctx.email, subject: "Hi", text: "Hello!" });
    return json({ sent: true }, null);
  } catch (e) {
    return json(null, { message: e.message });
  }
}
```

## You're safe from sneaky tricks 🛡️

You don't have to worry about people putting secret newlines in the subject to add
hidden recipients. The robot builds the email the safe way, so that trick simply
doesn't work.

## It shows up on the receipt 🧾

Each send is listed in `meta.mail_requests` (how many recipients, the size, whether it
was accepted). Great for double-checking.

**Next:** [Exact Decimal Math →](05-decimal.md)
