# mail Specification

## Purpose

The `mail` capability lets a handler send email through an operator-supplied SMTP relay via a
`mail.send({...})` JS global. Like `db`, it follows the trusted-connection model: the relay
host and credentials come from `config.mail` (operator-supplied, not script-controlled), so no
SSRF / private-IP guard is applied — internal and self-hosted relays are intended to work. Each
send is metered into `meta.mail_requests`. Rationale: `src/mail.rs`, `src/js/mail.js`,
`docs/04-mail.md`, and `CLAUDE.md`.

## Requirements

### Requirement: Opt-in injection via config.mail

The `mail` global SHALL be injected only when the request supplies a `config.mail` block; with
no `config.mail` the global is absent.

#### Scenario: Capability present with config

- **WHEN** a request includes a `config.mail` block and the handler reads `typeof mail`
- **THEN** `mail` is defined and exposes a `send` function

#### Scenario: Capability absent without config

- **WHEN** a request omits `config.mail` and the handler reads `typeof mail`
- **THEN** the result is `"undefined"`

### Requirement: Trusted operator-supplied connection

The relay connection SHALL be built from operator-supplied `config.mail` (`host`, `port`,
`user`, `password`, `tls`, `from`, `max_recipients`, `timeout_ms`) with no SSRF or private-IP
block applied to the target host.

#### Scenario: Connect to operator-named host

- **WHEN** `config.mail.host` names an internal or private relay
- **THEN** the connection is attempted without host allowlisting or private-IP rejection

#### Scenario: Transport security mode

- **WHEN** `config.mail.tls` is `"starttls"`, `"wrapper"`, or `"none"`
- **THEN** the transport uses STARTTLS, implicit-TLS (SMTPS), or plaintext respectively, defaulting to `"starttls"` on port `587`

#### Scenario: Optional authentication

- **WHEN** `config.mail.user` is empty
- **THEN** the connection is made without SMTP authentication

### Requirement: Send JS surface

The `mail` global SHALL expose `mail.send(opts)` accepting `from`, `to`, `cc`, `bcc`,
`reply_to`, `subject`, `text`, and `html`; `to`/`cc`/`bcc` each accept a single address string
or an array of address strings, and `from` defaults to `config.mail.from` when omitted.

#### Scenario: Single or list recipients

- **WHEN** `to` (or `cc`/`bcc`) is given as a single string or as an array of strings
- **THEN** each is normalized to a recipient list

#### Scenario: Default from address

- **WHEN** `send` is called without a `from`
- **THEN** the configured `config.mail.from` is used as the From address

#### Scenario: Body selection

- **WHEN** a send provides `text`, `html`, or both
- **THEN** a text-only, html-only, or `multipart/alternative` message is built respectively

### Requirement: Recipient validation

A send SHALL require at least one recipient and SHALL reject sends whose total recipients
(`to` + `cc` + `bcc`) exceed `config.mail.max_recipients` (default 50), and SHALL validate every
address.

#### Scenario: No recipients

- **WHEN** a send supplies no `to`, `cc`, or `bcc` addresses
- **THEN** `mail.send` throws an error (recipient required)

#### Scenario: Too many recipients

- **WHEN** the total recipient count exceeds `max_recipients`
- **THEN** `mail.send` throws an error reporting the count and the cap

#### Scenario: Invalid address

- **WHEN** any `from`/`to`/`cc`/`bcc`/`reply_to` address fails to parse as a mailbox
- **THEN** `mail.send` throws an error naming the offending field and value

### Requirement: Send outcome and error classification

On success `mail.send` SHALL return `{ accepted, response }`; on failure it SHALL throw a tagged
capability error whose code reflects the SMTP reply class — `MAIL_TRANSIENT` (4xx, retryable),
`MAIL_PERMANENT` (5xx, not retryable), or `MAIL_ERROR` (connect/TLS/usage, retryable).

#### Scenario: Accepted send

- **WHEN** the relay accepts the message
- **THEN** `mail.send` returns `{ accepted: true, response: <server reply line> }`

#### Scenario: Transient vs permanent failure

- **WHEN** the relay rejects with a 4xx reply versus a 5xx reply
- **THEN** the thrown error carries code `MAIL_TRANSIENT` (retryable) versus `MAIL_PERMANENT` (not retryable)

#### Scenario: Connection or usage failure

- **WHEN** the failure is a connect/TLS/IO error or a payload/validation error
- **THEN** the thrown error carries the fallback code `MAIL_ERROR`

### Requirement: Metering and operation cap

Each send SHALL be recorded into `meta.mail_requests` with its recipient count, serialized byte
size, and accepted flag, and SHALL be subject to the per-execution `max_ops` budget.

#### Scenario: Send recorded in meta

- **WHEN** a handler performs a `mail.send`
- **THEN** an entry appears in `meta.mail_requests` carrying `recipients`, `bytes`, and `accepted`

#### Scenario: Operation budget exhausted

- **WHEN** a send would exceed the per-execution `max_ops` budget
- **THEN** the call fails with code `MAIL_OP_LIMIT` (not retryable)
