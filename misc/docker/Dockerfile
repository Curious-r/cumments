FROM rust:1.88-alpine AS builder

# Install build dependencies
RUN apk add --no-cache build-base cmake clang musl-dev openssl-dev perl pkgconfig sqlite-dev

WORKDIR /usr/src/app

# Copy project files
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates

# Create a dummy main to allow cargo to download dependencies
RUN mkdir -p crates/cumments/src && \
    echo 'fn main() { println!("Dummy build"); }' > crates/cumments/src/main.rs

# Download and compile dependencies
RUN cargo build --release --bin cumments
RUN rm -rf target/release/deps/cumments*

# Copy actual source code
COPY . .

# Build the cumments binary
RUN cargo build --release --bin cumments

# Runtime stage
FROM alpine:3.20

# Install runtime dependencies
RUN apk --no-cache add ca-certificates sqlite-libs

WORKDIR /app

# Copy the binary from builder stage
COPY --from=builder /usr/src/app/target/release/cumments ./cumments
# Copy a runnable default configuration (logging mode; override via env vars)
COPY --from=builder /usr/src/app/misc/docker/config.toml ./config.toml

# Create data directory for SQLite database
RUN mkdir -p /app/data

EXPOSE 7931

# Set default log level
ENV RUST_LOG=info

CMD ["./cumments"]
