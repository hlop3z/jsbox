# ── Planner (cargo-chef) ─────────────────────────────────
FROM rust:1.92-alpine AS planner

RUN apk add --no-cache musl-dev
RUN cargo install cargo-chef --locked

WORKDIR /app
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

# ── Builder (cached deps + final build) ──────────────────
FROM rust:1.92-alpine AS builder

RUN apk add --no-cache musl-dev
RUN cargo install cargo-chef --locked

WORKDIR /app

# Cook deps from recipe (cached as long as deps don't change)
COPY --from=planner /app/recipe.json recipe.json
RUN cargo chef cook --release --target x86_64-unknown-linux-musl --recipe-path recipe.json

# Build the real app (only recompiles our code)
COPY . .
RUN cargo build --release --target x86_64-unknown-linux-musl \
    && strip target/x86_64-unknown-linux-musl/release/jsbox

# ── Runtime (distroless static — no glibc needed) ────────
FROM gcr.io/distroless/static-debian12:nonroot

WORKDIR /app

COPY --from=builder /app/target/x86_64-unknown-linux-musl/release/jsbox .
COPY config.example.json config.example.json

# Default config: bind 0.0.0.0 so container is reachable
COPY <<EOF /app/config.json
{"server":{"host":"0.0.0.0","port":3000}}
EOF

EXPOSE 3000

ENTRYPOINT ["./jsbox"]
