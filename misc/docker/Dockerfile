# === Stage 1: Planner ===
FROM lukemathwalker/cargo-chef:latest-rust-1-bookworm AS chef
WORKDIR /app

FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

# === Stage 2: Builder ===
FROM chef AS builder
WORKDIR /app

RUN apt-get update && apt-get install -y \
    pkg-config \
    libssl-dev \
    libsqlite3-dev \
    && rm -rf /var/lib/apt/lists/*

COPY --from=planner /app/recipe.json recipe.json
RUN cargo chef cook --release --recipe-path recipe.json

COPY . .

RUN cargo build --release --bin server

# === Stage 3: Runtime ===
# Use minimal Debian Slim
FROM debian:bookworm-slim AS runtime
WORKDIR /app

# Install runtime dependencies
# ca-certificates: Required for HTTPS connections to Matrix server
# libsqlite3-0: sqlx dynamically links system sqlite by default
RUN apt-get update && apt-get install -y \
    ca-certificates \
    libsqlite3-0 \
    libssl3 \
    && rm -rf /var/lib/apt/lists/*

# Copy compiled binary
COPY --from=builder /app/target/release/server /usr/local/bin/cumments-server

# Create data directory mount point
RUN mkdir -p /app/data

# === Environment Variables ===
# Log configuration
ENV RUST_LOG="server=info,adapter=info,storage=info"

# Database path (fixed path inside container)
ENV CUMMENTS_DATABASE_URL="sqlite://data/cumments.db"

# Network configuration (must listen on 0.0.0.0 inside container)
ENV CUMMENTS_HOST="0.0.0.0"
ENV CUMMENTS_PORT="3000"

EXPOSE 3000
VOLUME ["/app/data"]

CMD ["cumments-server"]
