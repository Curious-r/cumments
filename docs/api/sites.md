# Sites

Self-service site registration, domain verification and strict-mode secret
issuance. The trust model behind these operations is covered in
[Site trust](../site-trust.md) and the end-to-end walkthrough lives in
[Site verification](../site-verification.md).

## Register a site

`POST /api/v1/sites`

Registration is the mandatory first step for every site: a `site_id` that is
neither registered here nor declared in the `[sites]` configuration can never
accept writes. The request body is optional:

```json
{ "site_id": "my-blog" }
```

With a body, the caller picks the id (lowercase `[a-z0-9-]`, 1-64
characters); ids are first-come, and a conflict returns `409`. Without a
body, the server generates an unguessable random id (32 hex characters). The
chosen id is what shows up in Matrix: the Space alias
`#_cumments_my-blog:server`, each room alias
`#_cumments_my-blog_<post>:server` and the Space display name.
Chosen ids are a privilege: under the `optional` verification policy they
must verify at least one origin before they accept writes, while random ids
keep the relaxed migration behavior. The name stays reserved while
unverified (see [Site trust](../site-trust.md)).

The response carries the `site_id` and a one-time `claim_token`:

```json
{ "site_id": "my-blog", "claim_token": "..." }
```

The claim token proves ownership of the site and must be sent in the
`X-Cumments-Claim-Token` header for verification and secret issuance. It is
shown once and only its hash is stored.

## Start verification

`POST /api/v1/sites/{site_id}/verifications`

Headers: `X-Cumments-Claim-Token: <claim_token>`

Body:

```json
{
  "origins": ["https://blog.example.com"],
  "methods": ["well-known", "dns"]
}
```

`methods` are tried in order by `confirm`; publishing the same token in every
chosen location gives an automatic fallback. The response contains the token,
the expiry, and concrete publishing instructions.

## Confirm verification

`POST /api/v1/sites/{site_id}/verifications/confirm`

Body:

```json
{ "origin": "https://blog.example.com", "token": "..." }
```

Cumments fetches `{origin}/.well-known/cumments.json` and/or queries the
`_cumments.<host>` TXT record. On the first matching proof it records the
origin and returns the updated `verified_origins` list.

Well-known document shapes (both accepted):

```json
{ "site_id": "...", "token": "..." }
```

```json
{ "sites": [ { "site_id": "...", "token": "..." } ] }
```

DNS TXT value format:

```text
site_id=<site_id>,token=<token>
```

## Issue an HMAC secret (strict mode)

`POST /api/v1/sites/{site_id}/secret`

Headers: `X-Cumments-Claim-Token: <claim_token>`

Body: `{ "rotate": false }` (omit to issue; `true` replaces an existing
secret).

The site must be verified first. The secret is returned exactly once:

```json
{ "site_id": "...", "secret": "..." }
```

It is used as the HMAC key in edge-function deployments (see
[Site trust](../site-trust.md)); the same value must be set on the site
backend and used to sign every write request.
