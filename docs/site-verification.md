# Site verification and strict mode

This guide walks an SSG site owner through binding their site to Cumments and,
optionally, switching it to strict (HMAC) mode. The threat model behind these
two modes is explained in [site-trust.md](site-trust.md).

## The two modes

| Mode | What authenticates write requests | Where the secret lives |
|---|---|---|
| `origin` (default) | The browser `Origin` header, bound to a verified domain | Nowhere — proof is the domain itself |
| `secret` (strict) | HMAC signature over each request | A key only your site backend (edge function) holds |

Both still require the visitor PoW + Ed25519 signature for comment authorship.

## 1. Register the site

Registration is mandatory before the site can receive any comment: the
write path rejects unknown `site_id`s. You may either pick the id (first-come)
or let the server generate a random one.

```bash
curl -sS -X POST http://localhost:7931/api/v1/sites \
  -H "Content-Type: application/json" \
  -d '{"site_id":"my-blog"}'
```

The response contains the `site_id` and a one-time `claim_token`:

```json
{
  "site_id": "my-blog",
  "claim_token": "..."
}
```

Keep the claim token private: it proves ownership of this site. It is sent in
the `X-Cumments-Claim-Token` header for verification and secret issuance.
The chosen id is what appears in Matrix aliases
(`#_cumments_my-blog:server`) and the Space display name.

You can also register from a terminal with the CLI:

```bash
cumments sites register --site-id my-blog
```

## 2. Start verification

```bash
curl -sS -X POST http://localhost:7931/api/v1/sites/<site_id>/verifications \
  -H "X-Cumments-Claim-Token: <claim_token>" \
  -H "Content-Type: application/json" \
  -d '{
    "origins": ["https://blog.example.com"],
    "methods": ["well-known", "dns"]
  }'
```

`methods` are tried in order, so publishing the same token in both places
gives an automatic fallback (for example when the site is temporarily
unreachable but DNS is visible). The response includes the token, the expiry
and exact publishing instructions.

## 3a. Publish the well-known file (recommended for SSG)

Add `/.well-known/cumments.json` to the static output of your site build:

```json
{
  "site_id": "3f9c...",
  "token": "..."
}
```

For example:

- Hugo: `static/.well-known/cumments.json`
- Next.js: `public/.well-known/cumments.json`
- Eleventy: any directory copied verbatim to the output (e.g. `public/`), or a
  passthrough copy

Commit the file and deploy. Cumments fetches it over HTTPS and does not follow
cross-host redirects.

## 3b. Publish the DNS TXT record (alternative)

Add a TXT record at `_cumments.<host>` (for `blog.example.com` the record name
is `_cumments.blog.example.com`):

```text
site_id=<site_id>,token=<token>
```

DNS verification works even before the site is deployed, but propagation can
take minutes — wait, then re-run `confirm` (a confirm call may be repeated
until the token expires).

## 4. Confirm

```bash
curl -sS -X POST http://localhost:7931/api/v1/sites/<site_id>/verifications/confirm \
  -H "Content-Type: application/json" \
  -d '{
    "origin": "https://blog.example.com",
    "token": "<token from step 2>"
  }'
```

On success the response lists the site's `verified_origins`. The site is now
enforced: only requests whose `Origin` matches one of those origins can write
comments.

## 5. Optional: switch to strict mode

Strict mode replaces the Origin check with an HMAC signature, so even a
non-browser client cannot impersonate the site without your key. The site must
be verified first.

Issue the key (returns it exactly once):

```bash
curl -sS -X POST http://localhost:7931/api/v1/sites/<site_id>/secret \
  -H "X-Cumments-Claim-Token: <claim_token>" \
  -H "Content-Type: application/json" \
  -d '{}'
```

Store the returned secret in your edge function's environment (never in the
site bundle) and update your frontend to post comments to your own backend
instead of Cumments directly.

### Request signing

Every forwarded write request must carry:

- `X-Cumments-Timestamp`: Unix seconds
- `X-Cumments-Signature`: hex HMAC-SHA256 over
  `timestamp\nMETHOD\npath\nsha256_hex(body)`

with the key being the site secret. The timestamp must be within ±5 minutes.
The signature does not cover the `Host` header, so a secret must never be
shared between Cumments instances.

### Cloudflare Pages Functions

`functions/api/comments.js`:

```js
async function sign(secret, timestamp, method, path, body) {
  const key = await crypto.subtle.importKey(
    "raw", new TextEncoder().encode(secret),
    { name: "HMAC", hash: "SHA-256" }, false, ["sign"],
  );
  const bodyHash = hex(await crypto.subtle.digest("SHA-256", body));
  const message = `${timestamp}\n${method}\n${path}\n${bodyHash}`;
  const signature = await crypto.subtle.sign("HMAC", key, new TextEncoder().encode(message));
  return hex(signature);
}

function hex(bytes) {
  return [...new Uint8Array(bytes)].map((b) => b.toString(16).padStart(2, "0")).join("");
}

const CUMMENTS_BASE = "https://comments.example.com";
// Must be the Cumments endpoint your frontend posts to — the proxy's own
// route may differ and the signature covers this path, not the proxy path.
const CUMMENTS_PATH = "/api/v1/sites/<site_id>/posts/<post_slug>/comments";

export async function onRequestPost(context) {
  const body = await context.request.text();
  const timestamp = String(Math.floor(Date.now() / 1000));
  const signature = await sign(
    context.env.CUMMENTS_SITE_SECRET,
    timestamp,
    "POST",
    CUMMENTS_PATH,
    new TextEncoder().encode(body),
  );
  const upstream = await fetch(CUMMENTS_BASE + CUMMENTS_PATH, {
    method: "POST",
    headers: {
      "content-type": "application/json",
      "x-cumments-timestamp": timestamp,
      "x-cumments-signature": signature,
    },
    body,
  });
  return new Response(upstream.body, { status: upstream.status });
}
```

The same pattern works on Netlify (`netlify/functions/comments.mjs`, secret in
`CUMMENTS_SITE_SECRET`) and Vercel (`api/comments.mjs`). The path in the
signature must be the path of the **Cumments** endpoint, not the proxy path.

## Migrating from `optional` to `required`

`security.site_verification = "optional"` is the migration default: unverified
sites keep working so existing deployments are not broken overnight. To
tighten the instance:

1. Verify every site that must keep writing (steps 1–4), or declare them in
   `[sites."<id>"]` with `allowed_origins`.
2. Check the admin API for stragglers:
   `GET /api/v1/admin/sites` (with the admin token).
3. Flip the policy to `"required"` and restart. Unverified sites now receive
   `403 code=site-verification-required` on writes, and legacy auto-creation is
   disabled entirely.
