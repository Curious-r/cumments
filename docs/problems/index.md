# Problem types (RFC 9457)

Every error response uses the RFC 9457 problem details format with
`Content-Type: application/problem+json`:

| Member | Meaning |
|---|---|
| `type` | Canonical URI of the problem type. Resolves to the anchor on this page. |
| `title` | Stable short human-readable name of the type. |
| `status` | HTTP status code; always identical to the response's real status. |
| `detail` | Occurrence-specific explanation, intended for the client. |
| `code` | Short machine-readable slug; identical to the fragment (after `#`) of `type`. |
| `details` | Optional extension with additional structured data (e.g. validation errors). |

Clients should dispatch on `code` (or the `type` URI); `detail` is for
humans and must not be parsed.

## Invalid Proof-of-Work response {#invalid-pow}

- `type`: `https://curious-r.github.io/cumments/problems/#invalid-pow`
- `code`: `invalid-pow`
- `status`: `403`

The `challenge_response` did not satisfy the current PoW challenge. Fetch a
fresh challenge from `/api/v1/challenge`, solve it, and resubmit. A failed
PoW does not consume an `Idempotency-Key`.

## Invalid author signature {#invalid-signature}

- `type`: `https://curious-r.github.io/cumments/problems/#invalid-signature`
- `code`: `invalid-signature`
- `status`: `403`

The Ed25519 `author_signature` does not verify over the canonical message.
Sign the exact message documented for the operation and retry with the same
`Idempotency-Key` if this was a retry.

## Input validation failed {#validation-error}

- `type`: `https://curious-r.github.io/cumments/problems/#validation-error`
- `code`: `validation-error`
- `status`: `400`

One or more request fields failed validation. The `details` member carries
the per-field errors; correct them and resubmit.

## Resource not found {#not-found}

- `type`: `https://curious-r.github.io/cumments/problems/#not-found`
- `code`: `not-found`
- `status`: `404`

The route does not exist or the referenced resource (site, page, comment,
verification) is not visible to this request.

## Unauthorized {#unauthorized}

- `type`: `https://curious-r.github.io/cumments/problems/#unauthorized`
- `code`: `unauthorized`
- `status`: `403`

The presented author public key is not allowed to perform the operation
(e.g. editing or deleting a comment owned by another key).

## Comment not manageable {#not-manageable}

- `type`: `https://curious-r.github.io/cumments/problems/#not-manageable`
- `code`: `not-manageable`
- `status`: `403`

The target comment was posted by a Matrix user; manage it from a Matrix
client instead of the HTTP API.

## Method not allowed {#method-not-allowed}

- `type`: `https://curious-r.github.io/cumments/problems/#method-not-allowed`
- `code`: `method-not-allowed`
- `status`: `405`

The HTTP method is not supported on this route. Comment reads use `QUERY`,
writes use `POST`/`PUT`/`PATCH`/`DELETE`.

## Bad request {#bad-request}

- `type`: `https://curious-r.github.io/cumments/problems/#bad-request`
- `code`: `bad-request`
- `status`: `400`

The request is malformed or semantically invalid (bad JSON, missing
required field, invalid origin, etc.). The `detail` member explains the
specific problem.

## Conflict {#conflict}

- `type`: `https://curious-r.github.io/cumments/problems/#conflict`
- `code`: `conflict`
- `status`: `409`

The request conflicts with the current state of the resource.

## Rate limit exceeded {#rate-limited}

- `type`: `https://curious-r.github.io/cumments/problems/#rate-limited`
- `code`: `rate-limited`
- `status`: `429`

The per-client budget for this operation is exhausted. Wait and retry later;
do not keep retrying with the same `Idempotency-Key` expecting acceptance.
The response carries a `Retry-After` header set to the endpoint's fixed
window (3600 seconds for hourly limits, 60 seconds for the Operator API).

## Idempotency-Key required {#idempotency-key-required}

- `type`: `https://curious-r.github.io/cumments/problems/#idempotency-key-required`
- `code`: `idempotency-key-required`
- `status`: `400`

The `Idempotency-Key` header is mandatory on `POST`, `PUT`, `PATCH`, `DELETE`
and visitor media upload write submissions.

## Invalid Idempotency-Key {#invalid-idempotency-key}

- `type`: `https://curious-r.github.io/cumments/problems/#invalid-idempotency-key`
- `code`: `invalid-idempotency-key`
- `status`: `400`

The `Idempotency-Key` value must be 8-255 printable ASCII characters.

## Idempotency-Key reused {#idempotency-key-reused}

- `type`: `https://curious-r.github.io/cumments/problems/#idempotency-key-reused`
- `code`: `idempotency-key-reused`
- `status`: `409`

The `Idempotency-Key` was already bound to a different request fingerprint
(`METHOD\npath\nsha256(body)`). Use a fresh key for a different request, or
replay the exact original request.

## Site verification required {#site-verification-required}

- `type`: `https://curious-r.github.io/cumments/problems/#site-verification-required`
- `code`: `site-verification-required`
- `status`: `403`

The site is not verified and the global policy requires verification before
accepting writes. Complete the verification flow for the site first.

## Site not registered {#site-not-registered}

- `type`: `https://curious-r.github.io/cumments/problems/#site-not-registered`
- `code`: `site-not-registered`
- `status`: `404`

The `site_id` does not exist in the registry: it is neither registered
through the site API/CLI nor declared in the `[sites]` configuration.
Register it first with `POST /api/v1/sites` (or `cumments sites register`);
unregistered sites can never create a Matrix Space, regardless of the
verification policy.

## Site retired {#site-retired}

- `type`: `https://curious-r.github.io/cumments/problems/#site-retired`
- `code`: `site-retired`
- `status`: `410`

The site is being retired and no longer accepts writes. A background
pass is retiring its Matrix Space and rooms and clearing the local
projections; reads keep working until the local data is gone.

## Site origin denied {#site-origin-denied}

- `type`: `https://curious-r.github.io/cumments/problems/#site-origin-denied`
- `code`: `site-origin-denied`
- `status`: `403`

The request `Origin` is not allowed for this site (opaque `null` origins are
rejected outside `disabled` mode). Serve the page from an allowed origin or
use the site backend signature flow.

## Site signature invalid {#site-signature-invalid}

- `type`: `https://curious-r.github.io/cumments/problems/#site-signature-invalid`
- `code`: `site-signature-invalid`
- `status`: `403`

The `X-Cumments-Timestamp`/`X-Cumments-Signature` HMAC proof is missing,
stale, or does not match the request.

## Internal server error {#internal-error}

- `type`: `https://curious-r.github.io/cumments/problems/#internal-error`
- `code`: `internal-error`
- `status`: `500`

The server failed unexpectedly. The detail is intentionally generic; check
server logs for the real cause.
