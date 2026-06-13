# amq Specification

## Purpose

The `amq` capability exposes a `RabbitMQ` **producer** to the `QuickJS` sandbox so a handler can
publish messages to a broker. It follows the trusted-connection model of `db`/`mail`: the broker
is operator-supplied in `config.amq`, so there is no SSRF guard. It is producer-only (publish; no
consume/subscribe). The single JS surface is `amq.send(...)`, which publishes a batch in one
connection trip and meters message count and payload bytes. Rationale: `src/amq.rs`,
`src/js/amq.js`, and `docs/08-amq.md`.

## Requirements

### Requirement: Opt-in via config.amq

The `amq` global SHALL be injected only when the request supplies a `config.amq` block; otherwise
it does not exist in the sandbox.

#### Scenario: Capability present with config

- **WHEN** a request supplies a `config.amq` block
- **THEN** the handler can reference the `amq` global and call `amq.send(...)`

#### Scenario: Capability absent without config

- **WHEN** a request omits `config.amq`
- **THEN** `typeof amq === "undefined"` inside the handler

### Requirement: Trusted operator-supplied broker connection

The broker connection SHALL be taken from operator-supplied `config.amq` (`host`, `port`,
`username`, `password`, `vhost`, `exchange`) with no SSRF / private-IP guard, because the target is
operator-controlled rather than script-controlled.

#### Scenario: Connects to the configured broker

- **WHEN** `config.amq` names a host, port, and credentials
- **THEN** `amq.send` opens a connection to exactly that broker and authenticates with the supplied credentials, with no host allowlist or private-IP block applied

#### Scenario: Defaults applied to omitted fields

- **WHEN** `config.amq` omits `port`, `username`, `password`, `vhost`, or `exchange`
- **THEN** the defaults `5672`, `guest`, `guest`, `/`, and `""` (the default exchange) are used respectively

### Requirement: TLS via amqps

The capability SHALL connect over TLS (`amqps://`) when `config.amq.tls` is true, reusing the
`aws-lc-rs` rustls provider, and SHALL accept an optional `ca_cert` PEM path for a self-hosted
broker with a private certificate authority.

#### Scenario: TLS connection

- **WHEN** `config.amq.tls` is true
- **THEN** the connection to the broker is established over TLS

#### Scenario: Custom CA certificate

- **WHEN** `config.amq.tls` is true and `config.amq.ca_cert` names a PEM file
- **THEN** that CA is used to verify the broker certificate; omitting `ca_cert` relies on the bundled webpki roots

### Requirement: Batch publish via amq.send

`amq.send` SHALL accept a list of `[routingKey, payload]` pairs and publish every message in one
connection + channel trip, returning the number of messages published. A single
`["routingKey", payload]` pair SHALL be accepted and normalized to a one-element batch. Each
`payload` is published as its JSON-serialized bytes to the configured exchange using the pair's
routing key (the queue name under the default exchange).

#### Scenario: Publish a batch

- **WHEN** the handler calls `amq.send([["user.created", {id: 1}], ["user.created", {id: 2}]])`
- **THEN** both messages are published in one trip and the call returns `2`

#### Scenario: Single-pair shorthand

- **WHEN** the handler calls `amq.send(["emails", {to: "a@b.com"}])`
- **THEN** the single message is published and the call returns `1`

#### Scenario: Empty batch rejected

- **WHEN** the handler calls `amq.send` with no messages
- **THEN** the call fails with an `amq` error indicating at least one message is required

### Requirement: Batch size cap

The capability SHALL reject a single `send` whose message count exceeds `config.amq.max_batch`
(default 100) with a non-retryable `AMQ_BATCH_TOO_LARGE` error owned by the developer.

#### Scenario: Over-limit batch

- **WHEN** a `amq.send` batch contains more messages than `max_batch`
- **THEN** the call fails with error code `AMQ_BATCH_TOO_LARGE` and no messages are published

### Requirement: Error behavior on publish or connection failure

On a broker connection/authentication failure the capability SHALL throw a retryable
`AMQ_CONNECTION` error owned by the operator, and on a publish/protocol failure it SHALL throw a
retryable `AMQ_ERROR`; the thrown JS error carries the structured classification the engine
branches on.

#### Scenario: Broker unreachable

- **WHEN** the broker cannot be reached or authentication fails
- **THEN** `amq.send` throws an error with code `AMQ_CONNECTION` that is retryable and owned by the operator

#### Scenario: Publish failure

- **WHEN** a message fails to publish after the connection is open
- **THEN** `amq.send` throws an error with code `AMQ_ERROR` that is retryable

### Requirement: Metering into meta.amq_requests

Each `amq.send` call SHALL count as exactly one operation against the per-execution `max_ops`
budget regardless of batch size, and SHALL record a metric — including message count, total
payload bytes, duration, and whether the batch was published — into `meta.amq_requests`. Exceeding
`max_ops` SHALL fail the call with a non-retryable `AMQ_OP_LIMIT` error owned by the developer.

#### Scenario: One op per send

- **WHEN** a handler calls `amq.send` with a multi-message batch
- **THEN** exactly one operation is charged against `max_ops` and one entry is appended to `meta.amq_requests` carrying the message count and total payload bytes

#### Scenario: Operation cap exceeded

- **WHEN** a handler's `amq.send` call would exceed `max_ops`
- **THEN** the call fails with error code `AMQ_OP_LIMIT`, which is non-retryable and owned by the developer
