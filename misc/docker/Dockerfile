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

# Copy actual source code
COPY . .

# Build the cumments binary, then copy it out of the target cache mount.
# Cache-mount contents are discarded when the RUN finishes, so the binary
# must be placed somewhere that persists in this stage for the runtime COPY.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/src/app/target \
    cargo build --release --locked --bin cumments && \
    install -m 0755 target/release/cumments /usr/local/bin/cumments

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
    adduser -S -G cumments cumments && \
    mkdir -p /etc/cumments

# Copy the binary from builder stage
COPY --from=builder /usr/local/bin/cumments /usr/local/bin/cumments
# Copy the runnable example configuration as the default config
COPY --from=builder /usr/src/app/misc/config/config-example.toml /etc/cumments/cumments.toml

# Create the service data directory
RUN mkdir -p /srv/cumments && chown -R cumments:cumments /srv/cumments

USER cumments

WORKDIR /srv/cumments

EXPOSE 7931 3001

# Set default log level
ENV RUST_LOG=info

HEALTHCHECK --interval=30s --timeout=5s --start-period=15s --retries=3 \
    CMD wget -q -O /dev/null http://127.0.0.1:7931/health || exit 1

CMD ["cumments"]
