## MODIFIED Requirements

### Requirement: Per-partition fairness (Tier 5)

The system SHALL optionally cap concurrency per partition key so a noisy tenant cannot
monopolize a pod while global capacity remains. In trusted-header mode the partition key SHALL
be the request's trusted tenant identity, not any value the caller can assert.

#### Scenario: Noisy tenant sheds on its own share

- **WHEN** `max_concurrent_per_partition` is set and one tenant exceeds its share
- **THEN** that tenant's excess fast-fails HTTP 429 `PARTITION_OVERLOADED` (retryable, owner caller) while other tenants are unaffected

#### Scenario: Partition key is the trusted tenant identity

- **WHEN** trusted-header mode is enabled and a request carries a trusted tenant identity
- **THEN** fairness is enforced per that tenant identity and the resolved value is echoed in `meta.partition`

#### Scenario: Caller-asserted partition input is ignored

- **WHEN** a request supplies a partition via the `X-Partition-Key` header or `partition` body field in trusted-header mode
- **THEN** that caller-asserted value is ignored and cannot influence the fairness bucket

> **BREAKING / Migration**: The caller-asserted `X-Partition-Key` header / `partition` body
> source is removed in trusted-header mode (it let a caller pick or spoof its own bucket).
> Partitioning is now automatic per trusted tenant identity. In single-tenant/loopback mode
> behavior is unchanged.
