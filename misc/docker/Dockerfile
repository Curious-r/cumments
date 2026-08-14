# Build context must be the repository root:
#   docker build -f misc/docker/Dockerfile .

ARG RUST_VERSION=1.97.1
FROM rust:${RUST_VERSION}-alpine AS chef

# Install build dependencies
RUN apk add --no-cache build-base cmake clang musl-dev pkgconfig sqlite-dev

# cargo-chef keeps the dependency build in its own cached layer
RUN cargo install cargo-chef --locked --version 0.1.77

FROM chef AS planner
WORKDIR /usr/src/app
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    cargo chef prepare --recipe-path recipe.json

FROM chef AS builder
WORKDIR /usr/src/app
COPY --from=planner /usr/src/app/recipe.json recipe.json
# Compile all dependencies once; this layer is reused as long as the recipe
# (manifests + Cargo.lock) does not change.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    cargo chef cook --release --recipe-path recipe.json
# Copy the real sources and build the final binary
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    cargo build --release --locked --bin cumments

# Runtime stage
FROM alpine:3.22

# Install runtime dependencies. su-exec lets the entrypoint drop from root
# to the unprivileged service user after fixing volume ownership.
RUN apk --no-cache add ca-certificates sqlite-libs su-exec && \
    addgroup -S cumments && \
    adduser -S -G cumments cumments && \
    mkdir -p /etc/cumments

# Copy the binary from builder stage
COPY --from=builder /usr/src/app/target/release/cumments /usr/local/bin/cumments
# Copy the runnable example configuration as the default config
COPY --from=builder /usr/src/app/misc/config/config-example.toml /etc/cumments/cumments.toml
# Entrypoint fixes /srv/cumments ownership, then drops privileges.
COPY --chmod=0755 misc/docker/docker-entrypoint.sh /usr/local/bin/docker-entrypoint.sh

# Create the service data directory
RUN mkdir -p /srv/cumments

WORKDIR /srv/cumments

EXPOSE 7931 3001

# Set default log level; sqlx is quieted because it logs every SQL statement
# (including full statement text) at INFO.
ENV RUST_LOG=info,sqlx=warn

HEALTHCHECK --interval=30s --timeout=5s --start-period=15s --retries=3 \
    CMD wget -q -O /dev/null "http://127.0.0.1:${CUMMENTS__SERVER__PORT:-7931}/health" || exit 1

ENTRYPOINT ["/usr/local/bin/docker-entrypoint.sh"]
CMD ["cumments"]
