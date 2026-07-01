## 1. Trusted scope header ingress (runlet)

- [x] 1.1 Add a `scope` name to `TrustedHeaders` in `crates/runlet/src/config.rs` (default `x-tenant-scope`), matching the existing configurable-name pattern; extend the `TrustedHeaders::default()` and the header-name tests.
- [x] 1.2 Add a `scope: Option<String>` field to `TrustedIdentity` in `crates/runlet/src/identity.rs`, populated in `from_headers` from the configured scope header (trimmed, non-empty → `Some`); add a unit test that it reads only the configured name.

## 2. Fail-closed acting-org gate (runlet)

- [x] 2.1 In `crates/runlet/src/handler.rs::resolve_identity`, after the tenant-present check, reject a request whose `scope != Some("acting")` with `403 ACTING_SCOPE_REQUIRED` (reuse `identity_rejected`); order it alongside the anonymous/suspended/tenant-less rejects.
- [x] 2.2 Add the `ACTING_SCOPE_REQUIRED` code to the error taxonomy / docs where the other trusted-mode reject codes (`ANONYMOUS_FORBIDDEN`, `SUSPENDED_FORBIDDEN`, `TENANT_REQUIRED`) are defined.
- [x] 2.3 Unit-test the gate matrix in `resolve_identity`: `acting` proceeds; absent scope rejects; non-`acting` scope rejects; non-trusted mode ignores the scope header entirely.

## 3. Contract + docs

- [x] 3.1 Update `docs/design/nexus-upstream-requirements.md` (N5) with the concrete jsbox-side clause: the edge SHALL emit `x-tenant-scope: acting` for authorized-acting-org requests; note the box now enforces it fail-closed.
- [x] 3.2 Note the enforced assurance in `docs/design/multitenant-trust.md` (request pipeline + the acting-org gate) and record the producer-before-consumer bring-up ordering as a runbook note.

## 4. Integration test + gate

- [x] 4.1 Extend the trusted-mode integration coverage (`test_simple.py` trusted section): a request with `x-tenant-scope: acting` succeeds; one without it, and one with a non-`acting` value, are rejected `403 ACTING_SCOPE_REQUIRED`.
- [x] 4.2 Run the full gate in Docker (`cargo fmt --check`, `cargo clippy`, `cargo test`, supply-chain); confirm clippy clean and no new dependencies.
