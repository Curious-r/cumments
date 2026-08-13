# Development

## Toolchain

The workspace pins the Rust toolchain via `rust-toolchain.toml` (currently
1.97.1). CI runs:

```bash
cargo fmt --all -- --check
cargo check --locked --all-targets --all-features
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-targets --all-features
cargo test --locked --doc --all-features
```

CI also runs a syntax check of the demo's inline scripts with
`node --check`.

## CLI

```text
cumments appservice generate-registration [--server-name <domain>] [--url <url>] \
  [--output registration.yaml] [--quiet]
cumments backfill
cumments backup --output <file>
```

`--output` writes the real YAML with 0600 permissions. `--quiet` alone prints
an unusable `[REDACTED]` YAML and is only meant for demos/audits.

From the source tree, prefix any command with `cargo run -p cumments --`, e.g.
`cargo run -p cumments -- backfill`.

## Building the Docker image from main

The repository's `misc/docker/Dockerfile` builds from the local checkout:

```bash
docker build -f misc/docker/Dockerfile .
```

For a self-contained Dockerfile that clones the upstream `main` branch (useful
for testing before a release), keep these points in mind:

- A `git clone` inside a `RUN` step is cached by Docker by the command string:
  later builds reuse the first cloned snapshot. Bust the cache with
  `docker compose build --no-cache` or a build arg whose value changes every
  build (e.g. `--build-arg CACHEBUST=$(date +%s)`).
- `--mount=type=cache,target=/app/target` keeps the cargo `target/` directory
  in a cache mount. Cache-mount content is not part of the image, so copy the
  built binary out of the cache within the same `RUN`:

  ```dockerfile
  RUN --mount=type=cache,target=/usr/local/cargo/registry \
      --mount=type=cache,target=/app/target \
      cargo build --release --locked --bin cumments && \
      cp /app/target/release/cumments /app/cumments

  COPY --from=builder /app/cumments /usr/local/bin/cumments
  ```

## Testing locally without a homeserver

Set `mode = "logging"` in the config (see docs/configuration.md). The API and
the submission queue run, but nothing is written to Matrix and comments are not
projected back.
