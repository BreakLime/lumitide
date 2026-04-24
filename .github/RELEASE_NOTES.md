## What's new

### Lossless FLAC restored on Windows

Windows now streams and downloads **lossless FLAC** again. This works by replicating the Tidal desktop app's PKCE auth flow, which issues tokens with full lossless access. On login, a browser window opens automatically — just log in and Lumitide handles the rest.

Linux and macOS continue to use the device code login flow and receive **MP4 (HIGH quality / AAC)**. Native FLAC support for those platforms is planned.

### What changed under the hood

- **New auth flow (Windows):** PKCE OAuth with the Tidal desktop client — browser opens automatically, OS redirects back to Lumitide, no copy-pasting needed
- **AES-128-CTR stream decryption:** lossless streams are encrypted at rest; Lumitide now unwraps the per-track key and decrypts on the fly during download
- **Platform-split quality:** `LOSSLESS` requested on Windows, `HIGH` on Linux/macOS
- **End-to-end test:** a new ignored test (`e2e_stream_decrypt`) verifies the full pipeline from session load through decryption to a valid FLAC magic byte check and Symphonia probe

---
