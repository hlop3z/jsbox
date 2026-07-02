## 1. Event envelope + Sink port (runlet)

- [x] 1.1 Create `runlet/src/events.rs`: the versioned `Event` envelope `{ v, event_id, ts, tenant,
  user, plan, trace_id, type, body }` with a `serde` tagged `body` enum (`Usage` | `Audit`), the
  `usage` body (exec time, per-capability op counts, sizes, outcome) and `audit` body (decision +
  reason code + optional detail: plan/limit/usage, missing entitlement, scope). `event_id` = fresh
  v4 `uuid`.
- [x] 1.2 Define `trait Sink { fn record(&self, event: &Event); }` (non-blocking) and a `LogSink`:
  a bounded `mpsc` + a writer task that serializes each event as one JSON line to the dedicated
  stdout event stream. `record` does a non-blocking `try_send`; on a full channel, drop + bump a
  `dropped-events` counter. Provide a shutdown/flush handle (best-effort drain).
- [x] 1.3 Unit-test: envelope serializes with all fields + unique `event_id`; `try_send` on a full
  bounded channel drops and increments the counter (never blocks); disabled sink is a no-op.

## 2. Config + wiring (runlet)

- [x] 2.1 Add an `events` config block to `runlet/src/config.rs` (enabled on/off, channel bound,
  stream target/label), off by default. Unit-test parse/defaults.
- [x] 2.2 In `runlet/src/main.rs`, build the `LogSink` + writer task when enabled, store it in
  `AppState` (as a `dyn Sink` / `Option`), and flush it on the existing graceful-shutdown path.

## 3. Usage metering at the finish seam (runlet)

- [x] 3.1 Thread the `TrustedIdentity` values (tenant/user/plan) into `build_response` (Change B put
  them only on the span); reuse the `trace_id` already on `base_meta`.
- [x] 3.2 In `build_response`, after composing `meta`, emit one `usage` event (per executed request,
  any outcome) from the response facts + tenant/plan via the sink. No metric label added.

## 4. Audit at every gate (runlet)

- [x] 4.1 Add an audit helper (`audit_deny(sink, tenant, user, reason, detail, trace_id)` /
  `audit_allow(...)`) in `events.rs`/`handler.rs`.
- [x] 4.2 Emit a `denied` audit event at each `run_execute` reject site with its reason code
  (anonymous / suspended / tenant-less / non-acting scope / member-authz / quota / oversized /
  egress-session / shed), carrying tenant (when known) + user; fold the quota plan/limit/usage into
  the quota-denied event. Keep the aggregate `record_rejection()` metric.
- [x] 4.3 Emit an `allowed` audit event on the executed path (paired with the usage event) so every
  request produces exactly one audit event.
- [x] 4.4 Unit/flow-test the emit-count invariant: executed ⇒ 1 usage + 1 audit; rejected ⇒ 0 usage
  + 1 audit(denied,reason); no sensitive payload in any event.

## 5. Docs

- [x] 5.1 Document the `events` config, the event envelope + stdout stream, and the collector-routing
  in `docs/deployment.md`; note the durable-outbox-later seam (schema + `event_id`).
- [x] 5.2 Update `CLAUDE.md`'s `runlet` blurb: per-tenant usage + audit events (unified envelope,
  stdout stream, non-blocking/unsampled, identity in events not labels, outbox-later).

## 6. Integration test + gate

- [x] 6.1 Extend `test_simple.py` (trusted/telemetry-style dedicated box with `events` enabled to a
  captured stdout stream): an executed request yields one `usage` + one `allowed` audit event
  carrying tenant/plan/trace_id; a quota/scope/suspended rejection yields one `denied` audit event
  with the reason and no usage event; envelope fields + `event_id` uniqueness; request still succeeds
  when the event buffer is saturated (fail-open).
- [x] 6.2 Run the full gate in Docker: `cargo fmt --check`, `cargo clippy` (clean), `cargo test`,
  supply-chain (`cargo audit`/`deny`/`vet`); confirm no new dependencies and `cargo tree -i ring`
  unchanged.
- [ ] 6.3 Run `/opsx:sync` to fold the `tenant-metering` + `tenant-audit` specs into the main specs,
  then archive.
