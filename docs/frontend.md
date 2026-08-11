# Frontend integration

`misc/frontend/index.html` is a standalone demo styled as a real comment
section: posting, editing/deleting your own comments, pagination, SSE, and a
"My comments" management view, plus identity backup/restore. It defaults to
`http://localhost:7931`.

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

1. Call `GET /api/challenge`.
2. Find a `nonce` such that `SHA256(prefix + nonce)` starts with `difficulty`
   leading zero hex digits.
3. Submit `challenge_response = prefix + "|" + nonce`.
