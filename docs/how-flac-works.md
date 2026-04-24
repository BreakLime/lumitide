# How Lumitide gets lossless FLAC from Tidal

Tidal changed their streaming API in a way that broke lossless audio for every third-party client. This document explains what changed and how Lumitide fixed it.

---

## What broke and why

Third-party clients were using a public OAuth client (`fX2JxdmntZWK0ixT`) with a device code grant. The tokens it issues carry `"at": "BROWSER"` in the JWT payload. At some point Tidal started gating lossless streams on `"at": "INTERNAL"` tokens only — BROWSER tokens now silently fall back to MP4/AAC.

Most clients have open issues about this. The common workaround is to manually swap in client IDs scraped from other Tidal apps — brittle, and those IDs get rotated.

---

## Finding the right client

To get INTERNAL tokens we needed a client Tidal themselves control. The Windows desktop app was the obvious candidate — it's Electron, so traffic is easy to capture.

The captured auth request:

```
GET https://login.tidal.com/authorize
  ?client_id=mhPVJJEBNRzVjr2p
  &code_challenge=<S256 challenge>
  &code_challenge_method=S256
  &redirect_uri=tidal://login/auth
  &response_type=code
  &scope=r_usr+w_usr
```

No `client_secret` — this is a **PKCE** flow. The token response contains `"at": "INTERNAL"`. That's what unlocks lossless.

---

## The PKCE flow

PKCE (RFC 7636) is designed for native apps that can't safely store a secret:

1. Generate a random **code verifier**, hash it with SHA-256 → **code challenge**
2. Start the auth flow with the challenge
3. After login, exchange the auth code and the verifier for tokens — the server verifies `sha256(verifier) == challenge`

The tricky part is `redirect_uri=tidal://login/auth`. After login, the browser hands the `tidal://` callback to whatever is registered as the OS handler. On Windows, Lumitide temporarily registers itself in `HKCU\Software\Classes\tidal` (no admin rights needed), catches the callback, extracts the auth code, then deletes the key so the real Tidal app takes over again.

---

## The encryption: OLD_AES

With INTERNAL tokens, the playback endpoint returns a BTS manifest:

```json
{
  "encryptionType": "OLD_AES",
  "keyId": "<base64 security token>",
  "urls": ["https://lgf.audio.tidal.com/mediatracks/..."]
}
```

FLAC files on the CDN are **AES-128-CTR encrypted** — downloading without decrypting gives unplayable noise.

**Key unwrapping** — the `keyId` contains the per-track key and nonce, wrapped with AES-256-CBC using a master key that has been publicly known for years:

```
token = base64_decode(keyId)
iv    = token[0:16]
plain = AES-256-CBC-decrypt(master_key, iv, token[16:])
key   = plain[0:16]
nonce = plain[16:24]
```

**Stream decryption** — AES-128-CTR with the nonce as the upper 8 bytes of the counter, starting at zero. Cipher state is maintained across 64 KiB chunks so the counter increments correctly throughout the file.

---

## Platform support

| Platform | Auth flow | Quality |
|----------|-----------|---------|
| Windows  | PKCE (browser auto-redirect) | LOSSLESS FLAC |
| Linux / macOS | Device code | HIGH (MP4/AAC) |

FLAC on Linux/macOS requires registering a `tidal://` URI handler, which varies across desktop environments. It's on the roadmap.
