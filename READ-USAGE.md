# Use Cases

Real-world scenarios where a sandboxed JS execution engine fits.

---

## Workflow Automation & Integrations

- **Webhook processing** — Receive webhooks from Stripe, GitHub, Twilio, etc. and run custom response logic per event.
- **Scheduled / cron jobs** — A scheduler POSTs to `/execute` on a timer. Jobs are script rows in a database — no infra changes needed.
- **Event-driven automation** — Trigger scripts on signup, payment, deploy, or DB change. Send a Slack message, update a CRM, sync inventory.
- **Cross-service glue** — Bridge two SaaS products that lack native integration with a small script that talks to both APIs.
- **Email automation** — Generate and send welcome emails, digests, or alerts based on user activity or time-based rules.

## Business Rules & Decision Engines

- **Dynamic pricing** — Evaluate discount tiers, promo codes, or surge pricing formulas at request time without redeploying.
- **Loan / credit decisioning** — Run underwriting rules against applicant data. Business teams update rules through an admin UI.
- **Fraud detection** — Execute heuristics on transactions to flag suspicious patterns in real time.
- **Compliance & policy enforcement** — Encode regulatory rules as scripts that validate data submissions or financial transactions.
- **Eligibility & routing** — Determine support tier, workflow path, or product offering based on user attributes.

## Multi-Tenant SaaS Extensibility

- **Customer plugins** — Let SaaS customers write scripts that hook into your platform lifecycle (like Shopify Scripts or Salesforce Apex).
- **Computed fields** — Users define calculated fields via expressions (`ctx.price * ctx.qty * (1 - ctx.discount)`) — no schema changes.
- **Tenant-specific transforms** — Each customer defines how their imported/exported data maps to your schema.
- **White-label customization** — Resellers or enterprise tenants customize platform behavior without forking your codebase.
- **Extension marketplace** — Tenants publish and share scripts on an internal marketplace; sandbox guarantees platform stability.

## API Gateway & Middleware

jsbox covers the **programmable logic layer** of a gateway. Pair it with nginx/Caddy for TLS and raw proxying:

```
Client -> Nginx -> jsbox /execute -> upstream APIs
                      |
                      +-> db (auth, rate limits, config)
```

- **Auth & token validation** — Decode JWTs, validate API keys, check permissions against `db.query` — reject before hitting upstream.
- **Request transformation** — Reshape incoming payloads, inject headers, normalize formats before proxying via `api.post`.
- **Response aggregation** — Fan out to multiple upstreams with `api.get`, merge into one response, return to client.
- **Rate limiting** — Query a counter in DB, increment it, reject if over quota. Custom rules per tenant, endpoint, or plan tier.
- **Routing logic** — Script decides which upstream to call based on path, headers, tenant config, or A/B test assignment.
- **Request validation** — Validate body schema, required fields, content types before forwarding. Return structured errors.
- **API mocking** — Create mock endpoints for frontend dev or partner integration testing in seconds.
- **API versioning** — Script translates between API versions: v1 callers get responses reshaped from the v2 upstream.
- **Circuit breaking** — Track upstream error rates in DB; script returns a cached fallback when an upstream is degraded.
- **Multi-tenant gateway** — Each tenant gets their own gateway logic via per-request `config`. One jsbox instance, N tenants, zero leakage.

## AI & LLM Agent Integration

- **AI code interpreter** — LLM generates and executes JS to answer analytical questions, do math, or process data.
- **Agent tool execution** — AI agents call sandboxed functions as "tools" to query databases, hit APIs, or run calculations.
- **Prompt-driven automation** — User describes a task in natural language; LLM writes a script; sandbox runs it safely.
- **AI workflow nodes** — In visual AI builders, "Code" nodes let users insert custom logic between AI steps.
- **RL code evaluation** — Execute AI-generated code against test suites and feed results back as reward signals.

## Data Processing & Transformation

- **ETL pipelines** — Raw data in `context`, user-supplied mapping script, clean structured output. Each customer gets their own transform.
- **Stream processing** — Apply filtering, aggregation, or enrichment to real-time event streams.
- **Report generation** — Pull from multiple sources, apply calculations, return formatted results on demand.
- **Log processing & alerting** — Parse and filter logs, trigger alerts on user-defined conditions.
- **Data quality checks** — Run assertions against datasets to catch anomalies, missing values, or schema drift.

## PIM — Product Information Management

- **Catalog enrichment** — Script normalizes, validates, and enriches product data on import (unit conversion, slug generation, SEO fields).
- **Attribute computation** — Derive computed attributes from raw data (`ctx.width * ctx.height * ctx.depth` → shipping volume).
- **Cross-channel formatting** — Transform one product record into channel-specific shapes (Amazon, Shopify, wholesale PDF) via per-channel scripts.
- **Data quality gates** — Validate completeness before publish: reject products missing images, descriptions, or required attributes.
- **Supplier feed ingestion** — Each supplier sends a different CSV/JSON shape. Per-supplier scripts normalize into your canonical schema.

## DAM — Digital Asset Management

- **Upload processing** — On asset upload, script generates metadata: extracts dimensions from context, assigns tags, sets expiration dates.
- **Access control rules** — Script evaluates who can download what: check user role, asset license type, region restrictions via `db.query`.
- **Auto-tagging & classification** — Call an external AI tagging API via `api.post`, write results back to DB. Per-tenant tagging rules.
- **Asset transformation requests** — Script composes a transformation order (resize, watermark, format) and dispatches to a processing service via `api.post`.
- **Expiration & lifecycle** — Scheduled script queries DB for assets past retention date, flags for archival or deletion.

## CMS — Content Management

- **Dynamic page assembly** — Script fetches content blocks from DB, evaluates personalization rules (geo, segment, A/B), returns assembled page data.
- **Preview & publish logic** — Validation script checks content before publish: broken links, missing alt text, SEO score thresholds.
- **Content migration** — Transform content from one CMS schema to another during platform migration. Per-content-type mapping scripts.
- **Personalization rules** — Script decides which banner, CTA, or layout variant to serve based on visitor context.
- **Webhook-driven invalidation** — On content update webhook, script determines which CDN paths to purge and calls the purge API.

## CRM — Customer Relationship Management

- **Lead scoring** — Script evaluates lead attributes (company size, engagement, source) and computes a score. Rules update without deploys.
- **Contact enrichment** — On new contact, script calls enrichment APIs (`api.get` to Clearbit, etc.) and writes results to DB.
- **Assignment rules** — Script routes leads to sales reps based on territory, deal size, product interest, or round-robin from DB state.
- **Lifecycle triggers** — When a contact moves stages, script fires actions: send email via API, create task in project tool, update forecast in DB.
- **Duplicate detection** — Script queries DB for fuzzy matches on email/phone/company, returns merge candidates with confidence scores.

## ERP — Enterprise Resource Planning

- **Order validation** — Script checks inventory levels, credit limits, and shipping restrictions before order confirmation via `db.query`.
- **Invoice computation** — Calculate line items, taxes, currency conversion, and discounts. Each business unit gets its own tax script.
- **Approval workflows** — Script evaluates approval rules: purchase over $10k needs VP sign-off, cross-department transfers need finance review.
- **Inter-system sync** — Script bridges ERP and external systems: push orders to fulfillment API, pull tracking numbers back, update DB.
- **Financial close scripts** — Period-end scripts run accruals, reconciliations, or FX revaluation against DB with transaction support.
- **Custom reporting** — Script aggregates across tables (orders + invoices + payments), applies business logic, returns report-ready data.

## POS — Point of Sale & E-Commerce

- **Checkout logic** — Merchant-defined shipping rates, tax computation, discount rules (the Shopify Scripts model).
- **Product bundling** — Evaluate rules that auto-suggest bundles based on cart contents.
- **Seller automation** — Marketplace sellers define inventory sync, repricing, or order routing scripts.
- **Loyalty & rewards** — Compute points, tiers, and rewards based on program rules that change without deploys.
- **Receipt customization** — Script generates receipt data: applies store-specific formatting, promo messages, return policy text.
- **Inventory sync** — On sale, script decrements stock in DB and pushes update to e-commerce channel via `api.put`.
- **Multi-location pricing** — Script resolves price by store location, currency, and local tax rules from DB lookups.
- **Return & refund rules** — Script evaluates return eligibility: time window, item condition, customer history — returns approval or denial with reason.

## Testing, Validation & Monitoring

- **Input validation** — Server-side validation rules as JS. Share the same script between frontend preview and backend enforcement.
- **Synthetic monitoring** — Periodically run scripts that test your APIs and alert on failures or latency spikes.
- **Data migration scripts** — One-off backfills via `db.query` + `db.execute` with transaction support and automatic timeout.
- **Custom CI/CD steps** — Teams define build, test, or deploy steps as sandboxed scripts within a pipeline.

## Security & Observability

- **Custom WAF rules** — Security teams write request-inspection scripts that run at the API gateway.
- **Data redaction** — Apply PII masking or redaction rules to API responses or log streams.
- **Audit trail** — Every execution returns `meta` with timing, byte counts, and operation logs — built-in auditability.
- **Custom alerting** — Define alerting conditions that combine multiple metrics beyond what built-in tools offer.

## Configuration & Templating

- **Programmable config** — Scripts instead of static JSON/YAML to compute config values dynamically.
- **Template rendering** — Execute email, invoice, or notification templates with injected context data.
- **Feature flags with logic** — Evaluate complex targeting rules: user segment, time of day, percentage rollout, geography.
- **Dynamic form generation** — Scripts define form layouts, validation rules, or UI component trees based on context.

## Education & Playgrounds

- **Online code editors** — Browser-based REPLs with instant feedback, powered by a sandboxed backend.
- **Coding challenges** — Execute candidate-submitted code against test cases in isolation (HackerRank / LeetCode model).
- **Interactive tutorials** — Embed runnable code examples in docs or courses so learners can experiment safely.

## IoT & Device Management

- **Sensor event processing** — Custom logic when IoT devices report data (threshold alerts, anomaly detection, command dispatch).
- **Edge scripting** — Push user-defined scripts to gateways for local decisions without cloud round-trips.
- **Firmware rollout rules** — Define canary deployments or conditional rollouts by device attributes.

## Internal Tooling

- **Admin scripts** — Give ops/support a safe environment to run ad-hoc queries against production without SSH access.
- **Low-code workflows** — Visual builder where each node is a JS script. Output of one becomes `context` of the next.
- **Notification routing** — Evaluate rules to decide channel (email, SMS, push, Slack) and content per event.
