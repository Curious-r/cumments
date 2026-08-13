# Demo frontend

Cumments is a **backend-only** project: it exposes an HTTP API and SSE stream,
and speaks Matrix through its AppService. The repository does **not** ship a
production frontend. Website and SSG-template developers are expected to write
their own comment section for their own pages, using the
[HTTP API](api/index.md) (challenge, PoW, signing, comments, SSE).

`misc/demo/` is a standalone demo (no build step: `index.html` +
`demo.css` + `demo.js`) that exercises the full API: it connects to a real
backend and supports posting, editing and deleting your own comments,
pagination, nested replies, SSE live updates, and a "My comments" management
view. Use it as a reference implementation, not as a reusable component.

The demo posts directly to the API, so it works with `origin`-mode sites
(including unverified sites under the `optional` policy). In strict
(`secret`) mode the frontend must call its own backend instead — see
[site-verification.md](site-verification.md) for the edge-function pattern.
The target site must be registered first (every policy rejects unknown
`site_id`s): run `cumments sites register --site-id <id>` or
`POST /api/v1/sites`, then enter the same id in the demo settings.

## Running the demo

The demo has no build step. Open `misc/demo/index.html` in a browser and set
the API URL in the settings drawer (default `http://localhost:7931`). It loads
Tailwind, Markdown, DOMPurify and BIP39 from CDNs; BIP39 is fetched via
dynamic `import()` because static ES modules are blocked under `file://`. If
the CDN is unreachable the demo falls back to a random identity (see below).

If you open the file directly (`file://`), the browser sends `Origin: null`.
Cumments accepts that only under the dev-only
`security.site_verification = "disabled"` policy; with `optional` or
`required`, serve the demo over HTTP(S) instead.

The demo also needs a secure context for WebCrypto Ed25519: `file://` and
`http://localhost` work, but a LAN page served over plain `http://192.168.x.x`
or another non-localhost address will fail at identity creation. Serve the
demo over HTTPS unless you are testing on localhost.

One `file://` limitation remains: media URLs are returned as absolute
`/api/v1/media/...` paths, which resolve against the `file://` origin and
therefore do not load. Image/sticker/video/audio attachments are only visible
when the demo is served over HTTP(S). Everything else (API calls, SSE,
signatures, identity) works from a directly opened file.

The demo has a built-in language switcher (中文 / EN) in the top bar; the
choice is remembered in `localStorage` (`cumments_demo_lang`).

## Identity

Generate an Ed25519 keypair with WebCrypto and keep the private key in the
browser. The **public key is the identity**: send it as `author_public_key`,
and sign the canonical request message with the private key. Edit/delete are
authorized by comparing the presented public key to the one stored with the
comment and verifying the signature.

Identity recovery is mnemonic-first: a fresh identity is derived from a BIP39
12-word English mnemonic via SLIP-0010 at the fixed path `m/44'/1328'/0'`. The
mnemonic is not persisted across sessions — it lives only in the current tab's
session storage, is shown once at creation, and can be viewed again from the
settings drawer within the same session — so you must write it down. The
derived private key is cached in `localStorage`; clearing browser data removes
that cache, but the mnemonic is the offline backup — entering it again in the
settings drawer re-derives the exact same identity and writes it back. The
mnemonic itself is deliberately kept out of long-lived storage, so it stays
separate from the local cache (paper, a password manager, or another device).

As an advanced option, the settings drawer can export the identity as a JSON
file (`{version, publicKey, privateKey}`) and import it back; imports are
rejected when the private key does not match the stated public key. If the
BIP39 CDN is unreachable, the demo falls back to a random Ed25519 identity and
reminds you that mnemonic recovery is unavailable.

## Proof of work

1. Call `GET /api/v1/challenge`.
2. Find a `nonce` such that `SHA256(prefix + nonce)` starts with `difficulty`
   leading zero hex digits.
3. Submit `challenge_response = prefix + "|" + nonce`.

The canonical signing messages are documented in the
[API reference](api/index.md).
