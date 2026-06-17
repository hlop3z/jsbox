# Product Ideas — jsbox + Hasura + Postgres

Running list of product ideas for this tool. The recurring pattern:

- **Hasura** = data API + auth/permissions + durable event triggers + cron/scheduled triggers, all over Postgres.
- **jsbox** = the sandboxed compute: custom business logic, integrations (`http`), exact-decimal math (`$`/Decimal), DB access (`db`), and email (`mail`).
- **Postgres** = state (your tables + Hasura's own event/cron bookkeeping).

Division of labor: Hasura owns CRUD + durability + scheduling; jsbox runs the logic Hasura can't express. jsbox is to Hasura roughly what "activities" are to a workflow engine.

---

## Money & operations

- **Invoicing / quoting tool** — Hasura stores customers, line items, tax rules. jsbox computes totals/tax/discounts with exact decimals (no floating-point money bugs), generates the invoice payload, and on an event trigger emails it via `mail`. Cron trigger chases overdue invoices.
- **Subscription / dunning manager** — Hasura tracks plans and payment state; jsbox runs retry-and-escalate logic (charge via `http` to Stripe, on fail schedule a retry, send dunning email). Cron drives the schedule.
- **Commission / payroll calculator** — sales data in Postgres; jsbox runs per-rep commission rules (tiered, exact-decimal) too gnarly for SQL, writes results back. Rules differ per client → registered scripts per tenant.
- **Expense / approval routing** — submit an expense, event trigger runs a jsbox script that applies approval-routing rules (amount thresholds, category, manager chain) and notifies the right approver.

## Customer-facing / commerce

- **Lightweight booking / appointments** — availability + bookings in Hasura; jsbox does slot conflict-checking, buffer rules, reminder scheduling (cron), confirmation emails.
- **E-commerce backend for a niche store** — Hasura is the catalog/orders API; jsbox handles checkout logic, shipping-rate lookups (`http` to a carrier), inventory decrement, order-confirmation mail. Event-chained: order created → fulfillment script → notify.
- **Loyalty / points engine** — every purchase event triggers a jsbox script applying points rules (exact-decimal balances), tier upgrades, reward issuance.
- **Form-to-workflow builder** — public forms write to Hasura; each submission fires a jsbox script that validates, enriches (geocode/email-verify via `http`), routes, and notifies. A Typeform+Zapier for one vertical.

## Integration & automation (high value, low effort here)

- **Per-tenant integration / webhook hub** — let SMB customers write small jsbox scripts to transform/route their own data between systems. Safe "run my JS on this event" without exposing your infra. **jsbox's killer differentiator.**
- **Internal "Zapier-lite"** — Hasura event triggers + jsbox scripts = no-infra automation: "when a deal closes, create a project, message the team, schedule onboarding." Choreography via event chaining.
- **Data sync / ETL connector** — cron-driven jsbox scripts pull from a customer's API (`http`), normalize, upsert into Postgres via Hasura. Sold as "connect your X to your Y."

## Reporting & comms

- **Automated reporting / digests** — cron trigger → jsbox aggregates the week's data, formats a summary, emails it. "Monday morning numbers" for owners.
- **Notification / alerting service** — define thresholds in Hasura; event triggers run jsbox scripts that evaluate rules and fan out emails/SMS (via `http` to Twilio).
- **Document generation** — contracts, certificates, receipts: jsbox merges data into a template and returns the payload; Hasura stores and serves.

## Vertical micro-SaaS (where the real money is)

Pick one industry and combine the above:

- **Field-service / trades** (HVAC, cleaning, landscaping): jobs, scheduling, invoicing, reminders.
- **Salon / clinic**: bookings, no-show reminders, loyalty, intake forms.
- **Property management**: rent tracking, maintenance request routing, lease-renewal cron, owner reports.
- **Gyms / studios**: memberships, class booking, dunning, attendance rules.
- **Restaurants / catering**: orders, quotes, scheduling, supplier reorder alerts.

---

## The pattern that makes this stack special

The standout category is **programmable automation** (the integration/automation section). Most SMB tools are rigid, and most "let customers add logic" platforms are a security nightmare. jsbox's sandbox lets you safely expose _user-written logic_ — so the differentiated products are the ones where SMB customers (or their power users) write small scripts to bend the tool to their workflow, while Hasura keeps the data and durability boring and solid.

## Notes on durability (context for the above)

- Hasura **event triggers** give durable, at-least-once, retried delivery on Postgres — effectively the outbox pattern, built and operated for you. Make jsbox scripts **idempotent**.
- Hasura **cron / scheduled triggers** cover recurring and one-off future jobs (also persisted + retried).
- This gives **choreography** (each step reacts to events), not **orchestration** (a conductor with replay/saga/compensation). Reach for DBOS/Temporal/Inngest _on top of the same Postgres_ only when you hit genuine orchestration needs (sagas, compensation, long durable waits).

## Trust boundaries

- **Hasura → jsbox**: authenticate the webhook with the shared secret (jsbox `/execute` auth) so only Hasura can invoke scripts.
- **jsbox → data**: via `db` (operator-supplied, trusted, bypasses Hasura permissions) for fully-trusted scripts, or via `http` back to Hasura's GraphQL (SSRF-guarded, honors row-level permissions) when permissions must be enforced. Allowlist the Hasura host for the `http` path.

---

# Round 2 — more ideas

Several of these lean into jsbox's sandbox as the *product itself*, not just glue.

## AI / agent infrastructure (very timely)

- **⭐ Code-interpreter-as-a-service** — let AI agents run model-generated JS safely. jsbox is the sandbox; Hasura stores sessions/results. Hot need right now (every agent framework wants safe code execution) and jsbox is purpose-built for it.
- **Safe "tool execution" backend for LLM apps** — an LLM emits a tool call → your app runs the corresponding jsbox script. Sandboxed tool-calling without each customer self-hosting a runtime.
- **Prompt/response post-processing pipelines** — jsbox scripts validate, redact PII, reshape, or score LLM output before it reaches the user; Hasura logs every step for audit.

## Rules / pricing / policy engines (sell "logic you can change without a deploy")

- **⭐ Dynamic pricing engine** — per-customer/per-tenant pricing rules as editable jsbox scripts (exact-decimal). Change pricing without shipping code. Marketplaces, SaaS, travel.
- **Eligibility / underwriting rules** — insurance, lending, discounts, promos: rules as scripts, decisions logged in Hasura for audit. Regulated SMBs love the audit trail.
- **Fees / tax / surcharge calculator API** — a single endpoint other apps call; the math lives in versioned jsbox scripts.

## Trust, compliance, audit

- **Tamper-evident audit log service** — persist every jsbox `{data, error, meta}` execution in Hasura as an immutable decision log. Sell "explainable automation" to compliance-sensitive SMBs (clinics, finance, legal).
- **Data-validation / quality gateway** — incoming records run through jsbox validation rules before landing in Postgres; rejects routed for review. "Schema + business rules at the door."
- **Consent / preferences enforcement** — jsbox scripts evaluate consent rules before any send/share action; Hasura is the consent store.

## Platform / white-label plays

- **⭐ "Functions" feature for an existing SaaS** — jsbox bolts a safe "write a script to extend this" feature onto any Hasura-backed app. Sell jsbox as the extensibility layer other SaaS products embed.
- **Customer-facing API gateway** — let your SMB customers expose *their* data via an API, with jsbox scripts as the per-customer transform/auth logic. White-label "give your users an API."
- **Multi-tenant webhook signer/verifier** — jsbox handles per-tenant signing secrets, HMAC verification, payload transforms; Hasura stores tenant configs.

## Operational / back-office

- **SLA / escalation monitor** — cron + event triggers run jsbox scripts that check ticket/order ages against SLA rules and escalate. Vertical-agnostic.
- **Inventory reorder / supplier automation** — stock crosses a threshold → jsbox computes reorder qty (lead-time, EOQ math) → drafts a PO email. Retail/restaurants/trades.
- **⭐ Reconciliation engine** — nightly cron: jsbox compares two data sources (bank vs ledger, Stripe vs orders), flags mismatches into Hasura. Bookkeepers pay for this.
- **Onboarding / offboarding automation** — new employee/customer event fans out into provisioning steps (accounts, emails, checklists) via event-chained scripts.

## Consumer-ish / niche

- **Personalized notification engine** — jsbox evaluates per-user rules ("alert me when X") on event triggers; price-drop alerts, restock, deadlines.
- **Gamification / quests layer** — drop-in points/badges/streaks engine other apps call; rules as scripts so each customer customizes.

## ⭐ Top bets

The throughline: **sell the sandbox.** These monetize jsbox's actual moat — safely running untrusted/customer-written logic — rather than treating it as plumbing behind a vertical app.

1. **Code-interpreter-as-a-service** for AI agents — biggest tailwind, most direct fit.
2. **"Functions" extensibility layer** embedded in other SaaS — recurring B2B revenue, sticky.
3. **Dynamic pricing / rules engine** — clear pain, exact-decimal is a real differentiator.
4. **Reconciliation engine** — boring, valuable, bookkeepers pay reliably.
