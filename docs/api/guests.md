# Guests

Public self-service reads for guest (visitor) identities on a site.

## Get the guest's current profile

`GET /api/v1/sites/{site_id}/guests/profile?author_public_key=...`

Returns the guest's current global Matrix profile for this site. The virtual
user is derived from `site_id + author_public_key`, so this endpoint answers
"who am I on this site?" for a browser-held key without any session.

Response:

```json
{
  "guest_id": "a1b2c3d4e5f60718a1b2c3d4e5f60718",
  "display_name": "Alice",
  "avatar_url": "https://comments.example.net/api/v1/media/..."
}
```

- `display_name` is the current profile display name, or `null` when unset.
- `avatar_url` is a signed proxy URL (96×96 crop variant when the media
  proxy is enabled; the raw `mxc://` URL otherwise), or `null` when unset.
- Unknown virtual users and homeservers configured not to disclose profiles
  (`403`, MSC4170) both return an **empty profile** (`null` fields) with
  `200`, so clients treat "no profile" as a normal state.

The endpoint is public and read-only: the Ed25519 public key is the
identity, it is high-entropy and not enumerable, and any avatar is already
public through the guest's comments. Requests are rate limited per client
IP (default 120/hour, configurable via `rate_limit.guest_profile`).

Errors: `404` when the site is not registered (the parent resource does not
exist), `400` for missing/invalid `author_public_key`, `429` when rate
limited.
