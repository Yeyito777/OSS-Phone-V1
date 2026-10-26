# Fonts

Moon and the System, Phone and Messages apps draw text with the embedded
Inter (SIL OFL 1.1). Anything Inter lacks falls back, in order, to:

| File | Covers |
| --- | --- |
| `NotoSans-Regular.ttf`, `NotoSans-SemiBold.ttf` | Latin extended, Greek, Cyrillic, Vietnamese (SemiBold for SemiBold and Bold text) |
| `NotoSansSymbols2-Regular.ttf`, `NotoSansSymbols-Regular.ttf` | dingbats, arrows, misc symbols |
| `NotoSansMath-Regular.ttf` | math operators (∞, ≠, …) |
| `NotoColorEmoji.ttf` | colour emoji, including ZWJ sequences, skin tones, keycaps and flags (its ligatures) |

All SIL OFL 1.1; the licence texts ship beside them. `scripts/a26-shell/fonts.manifest`
pins them by SHA-256; `fetch-fonts.sh` downloads them, `build.sh` puts them in
the image (`share/fonts`, about 14 MB) and `deploy.sh` pushes any that differ
to `/opt/a26-shell/share/fonts` (chroot; `MOON_FONTS` overrides it). The files
are memory-mapped the first time a character needs them, so they cost no
memory until then; a first fallback glyph costs ~7 ms (the face), a first
emoji ~0.5 ms (PNG decode and downscale), then both are cached.

Not covered: CJK, Arabic, Hebrew, Indic and other scripts that need shaping
or bidi; they draw as empty boxes, never as '?'.
