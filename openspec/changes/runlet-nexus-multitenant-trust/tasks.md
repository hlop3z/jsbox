## 1. Trusted-identity config + boot guard (runlet)

- [x] 1.1 Add trusted-header mode to `runlet/src/config.rs`: configurable header names (tenant/user/roles/entitlements/suspended/anonymous, defaults `x-tenant-id`/`x-user-*`), an `assert_network_isolation` opt-in, and repurpose `access_token` as the edge service credential.
- [x] 1.2 Extend the existing boot guard: refuse to start in trusted-header mode on a non-loopback bind unless isolation is asserted (mirror the `allow_unauthenticated` guard); add unit tests for the refuse/allow matrix.
- [x] 1.3 Add a `TrustedIdentity` type + extractor (tenant id, user id, roles, entitlements, suspended, anonymous) that reads only the configured trusted headers and ignores client-supplied identity.

## 2. Identity enforcement at /execute (runlet)

- [x] 2.1 In `runlet/src/handler.rs`, resolve `TrustedIdentity` before body work; require a tenant id for tenant-scoped requests (reject when absent in trusted mode).
- [x] 2.2 Reject `x-auth-anonymous: true` and `x-user-suspended: true` with an authorization failure before any handler runs; add the error codes to the taxonomy.
- [x] 2.3 Enforce the edge service credential on inbound requests when configured; redact it in logs.

## 3. Tenant becomes the fairness + cache key (runlet-core)

- [x] 3.1 Source the Tier 5 `PartitionLimiter` key from the trusted tenant id; remove the `X-Partition-Key` header / `partition` body source in trusted mode; keep `meta.partition` echo.
- [x] 3.2 Namespace the bytecode/compilation cache by the trusted tenant id (replace the current partition namespace source).
- [x] 3.3 Update/extend unit tests: noisy-tenant shedding by tenant id, caller-asserted partition is ignored, no cross-tenant cache dedup.

## 4. Tenant-scoped egress (fabric-wire + fabricd)

- [x] 4.1 Extend `WireInit` in `crates/fabric-wire/src/wire.rs` to carry the trusted tenant id (serde default + skip-if-none; never sourced from script).
- [x] 4.2 In `runlet` `sidecar`/`connect_session`, populate the tenant id on the handshake for tenant-scoped sessions.
- [x] 4.3 In `fabricd`, scope resource resolution to the session tenant's binding set; refuse names outside it; keep credentials in `fabricd` (unchanged wire result).
- [x] 4.4 Extend the operator resource config (`fabric-backends` `ResourceBinding`) to associate bindings with a tenant; add resolution unit tests (in-tenant resolves, cross-tenant refused).

## 5. Coarse member authorization (runlet)

- [x] 5.1 Add a per-capability role/entitlement gate keyed off `x-user-roles`/`x-user-entitlements`; reject a member lacking the required entitlement before the capability runs.
- [x] 5.2 Make the capability→required-entitlement mapping config-driven; unit-test permit/deny.

## 6. Per-tenant quota + accounting (runlet)

- [x] 6.1 Add a `plan → limit` quota engine modeled on nexus `routing-rs/plan.rs` (`PlanLimits`/`DomainLimit`/`QuotaExceeded`, at-or-above, fail-closed default for unknown/empty plan).
- [x] 6.2 Attribute usage to the trusted tenant id; enforce the hard cap in `runlet`; return the structured over-limit result.
- [x] 6.3 Unit-test the gate matrix (within/at/over limit, unknown plan → most restrictive, empty config denies).

## 7. Deploy + docs

- [x] 7.1 Add a k8s NetworkPolicy restricting `pool_jsbox` to the edge; document the trust invariant (reachable only via edge) in `docs/design/`.
- [x] 7.2 Update `CLAUDE.md` crate blurbs + `docs/` for the trusted-identity contract and tenant keying.
- [x] 7.3 Record the nexus upstream requirement (N5: identity plane must emit the authorized acting org, not the home org) in the nexus repo's `nexus-upstream-requirements.md` (cross-repo, tracked as a dependency).

## 8. Verify

- [x] 8.1 Integration test: `runlet` behind trusted headers — tenant isolation of fairness/cache, suspended/anonymous rejection, tenant-scoped egress (in-tenant resolves, cross-tenant refused), quota over-limit.
- [x] 8.2 Run the full gate in Docker (`cargo fmt --check`, `cargo clippy`, `cargo test`, supply-chain); confirm `cargo tree -i ring` unchanged and clippy clean on the gate.
