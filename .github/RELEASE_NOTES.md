## What's new

### Tidal API compatibility fix

Tidal no longer serves lossless FLAC to third-party clients — audio is now delivered as MP4 (HIGH quality). v1.4.1 fully adapts to this:

- Playback streams MP4 correctly without stalling
- Downloads detect the actual format from the file and save as `.m4a`
- Metadata (tags + cover art) is embedded correctly for both FLAC and MP4 files
- The "already downloaded" check now recognises both `.flac` and `.m4a` files

**If you are on v1.4.0 or earlier, update now — playback and downloads will fail on older versions.**

---
