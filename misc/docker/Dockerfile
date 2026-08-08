# Build context must be the repository root:
#   docker build -f misc/docker/Dockerfile .

ARG RUST_VERSION=1.97
FROM rust:${RUST_VERSION}-alpine AS builder

ARG VERSION=0.17.0
ARG REVISION=unknown

LABEL org.opencontainers.image.title="Cumments"
LABEL org.opencontainers.image.description="Decentralized comment system backend on Matrix"
LABEL org.opencontainers.image.source="https://github.com/Curious-r/cumments"
LABEL org.opencontainers.image.version="${VERSION}"
LABEL org.opencontainers.image.revision="${REVISION}"

# Install build dependencies
RUN apk add --no-cache build-base cmake clang musl-dev pkgconfig sqlite-dev

WORKDIR /usr/src/app

# Copy project files
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates

# Create a dummy main to allow cargo to download dependencies
RUN mkdir -p crates/cumments/src && \
    echo 'fn main() { println!("Dummy build"); }' > crates/cumments/src/main.rs

# Download and compile dependencies
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/src/app/target \
    cargo build --release --locked --bin cumments
RUN rm -rf target/release/deps/cumments*

# Copy actual source code
COPY . .

# Build the cumments binary
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/src/app/target \
    cargo build --release --locked --bin cumments

# Runtime stage
FROM alpine:3.22

ARG VERSION=0.17.0
ARG REVISION=unknown

LABEL org.opencontainers.image.title="Cumments"
LABEL org.opencontainers.image.description="Decentralized comment system backend on Matrix"
LABEL org.opencontainers.image.source="https://github.com/Curious-r/cumments"
LABEL org.opencontainers.image.version="${VERSION}"
LABEL org.opencontainers.image.revision="${REVISION}"

# Install runtime dependencies
RUN apk --no-cache add ca-certificates sqlite-libs && \
    addgroup -S cumments && \
    adduser -S -G cumments cumments

WORKDIR /app

# Copy the binary from builder stage
COPY --from=builder /usr/src/app/target/release/cumments ./cumments
# Copy a runnable default configuration (logging mode; override via env vars)
COPY --from=builder /usr/src/app/misc/docker/config.toml ./config.toml

# Create data directory for SQLite database
RUN mkdir -p /app/data && chown -R cumments:cumments /app

USER cumments

EXPOSE 7931 3001

# Set default log level
ENV RUST_LOG=info

HEALTHCHECK --interval=30s --timeout=5s --start-period=15s --retries=3 \
    CMD wget -q -O /dev/null http://127.0.0.1:7931/health || exit 1

CMD ["./cumments"]
