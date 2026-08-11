# Quick start

This guide runs a complete local stack with the official images: a
[tuwunel](https://github.com/matrix-construct/tuwunel) homeserver and Cumments
as its Matrix Application Service. The ready-made compose file lives at
[`misc/docker/compose.yaml`](../misc/docker/compose.yaml) and is a minimal,
self-contained example: both services are configured entirely through
environment variables; the only manual step outside the compose file is
generating the AppService registration and copying its tokens in.

## Prerequisites

- Docker with Compose (v2).
- `curl`, for creating the first Matrix account.

## 1. Copy the compose file

Copy the example into a directory of its own so the generated
`registration.yaml` does not end up inside the repository:

```bash
mkdir -p ~/cumments-demo && cd ~/cumments-demo
cp /path/to/cumments/misc/docker/compose.yaml docker-compose.yml
```

For a local test the defaults work as-is: the Matrix server name is
`localhost:8008`, tuwunel is published on port `8008`, and the Cumments API on
port `7931`. For a real deployment, create a `.env` file next to the compose
file before generating the registration:

```dotenv
MATRIX_DOMAIN=matrix.example.com
```

`MATRIX_DOMAIN` is the Matrix server name, i.e. the part after `:` in user IDs
and aliases. Both services read it from the same variable, so changing it in
one place keeps them consistent.

## 2. Generate the AppService registration

```bash
docker run --rm --entrypoint cumments \
  ghcr.io/curious-r/cumments:latest \
  generate-registration \
  --server-name localhost:8008 \
  --url http://cumments:7931 > registration.yaml
```

- `--server-name` must equal `MATRIX_DOMAIN` (`localhost:8008` for the local
  default).
- `--url` is the callback URL the homeserver uses to push events. Inside
  Compose this is the service name, `http://cumments:7931`; behind a reverse
  proxy use the public URL instead.
- The YAML is written to stdout and saved as `registration.yaml`. The matching
  `as_token` and `hs_token` are printed to stderr — copy them for the next
  step.

## 3. Fill in the tokens

Open `docker-compose.yml` and replace the two placeholders:

```yaml
      CUMMENTS__MATRIX__APPSERVICE__AS_TOKEN: "<as_token>"
      CUMMENTS__MATRIX__APPSERVICE__HS_TOKEN: "<hs_token>"
```

The same file is mounted into both containers: tuwunel loads it as its
AppService registration, and Cumments validates its configuration against it at
startup, so a typo or a mismatched token fails fast.

## 4. Start the stack

```bash
docker compose up -d
docker compose logs -f tuwunel
docker compose logs -f cumments
```

Cumments should log `Configuration loaded successfully.`, `Database
initialized.`, and `Server listening on 0.0.0.0:7931`.

> The example intentionally keeps registration open on tuwunel so the first
> account can be created with one request. Set `TUWUNEL_REGISTRATION_TOKEN`
> and remove
> `TUWUNEL_YES_I_AM_VERY_VERY_SURE_I_WANT_AN_OPEN_REGISTRATION_SERVER_PRONE_TO_ABUSE`
> before exposing the homeserver beyond your machine.

## 5. Create the admin account

The first account registered on tuwunel is granted admin privileges. The
compose file expects it to be `admin`:

```bash
curl -sS -X POST http://localhost:8008/_matrix/client/v3/register \
  -H 'Content-Type: application/json' \
  -d '{"username":"admin","password":"your-admin-password","auth":{"type":"m.login.dummy"}}'
```

The response contains `user_id`, `device_id`, and `access_token`. If you pick a
different username, update
`CUMMENTS__MATRIX__MODERATION__ADMIN_ID` (default
`@admin:localhost:8008`) in the compose file and restart Cumments:

```bash
docker compose up -d --force-recreate cumments
```

## 6. Verify

1. Open the [demo frontend](demo.md) (`misc/demo/index.html`) against
   `http://localhost:7931` and post a comment.
2. In Matrix, check that a Space (`Comments: <site>`), a comment room
   (`Comments: <site>/<post>`), and the virtual user were created.
3. The comment should appear in the frontend in real time via SSE.

If comments exist in Matrix but not in the API, rebuild the read model from
Matrix history:

```bash
docker compose exec cumments cumments backfill
```

## Optional: room version 12

Room version 12 hardens rooms (hash-based room IDs, immutable creator power).
On tuwunel, set `TUWUNEL_DEFAULT_ROOM_VERSION: "12"` in the compose
environment, or request v12 per room with
`CUMMENTS__MATRIX__APPSERVICE__ROOM_VERSION: "12"` (see
[configuration.md](configuration.md)).

## Troubleshooting

- **Compose refuses to start**: the `./registration.yaml` bind mount does not
  exist yet. Run step 2 first.
- **`as_token` / `hs_token` mismatch**: the values in the compose file do not
  match the registration YAML. Regenerate both together, or copy them from the
  stderr output of step 2.
- **Cumments logs `invalid hs_token`**: the token registered on the homeserver
  differs from `CUMMENTS__MATRIX__APPSERVICE__HS_TOKEN`; fix and restart both
  services.
- **Comments exist in Matrix but not in the API**: the push queue was blocked
  or a transaction was never acked; restart the service and, if needed, run
  `cumments backfill`.
- **The server name changed after first start**: tuwunel stores the server name
  in its database and cannot change it later. Remove the `tuwunel-data` volume
  (`docker compose down -v`) and start over.
