FROM rust:1.80-alpine AS builder

# Install build dependencies
RUN apk add --no-cache musl-dev openssl-dev pkgconfig sqlite-dev

WORKDIR /usr/src/app

# Copy project files
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
COPY migrations ./migrations

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
FROM alpine:latest

# Install runtime dependencies
RUN apk --no-cache add ca-certificates openssl sqlite-libs

WORKDIR /app

# Copy the binary from builder stage
COPY --from=builder /usr/src/app/target/release/cumments ./cumments

# Create data directory for SQLite database
RUN mkdir -p /app/data

EXPOSE 3000

# Set default log level
ENV RUST_LOG=info

CMD ["./cumments"]
