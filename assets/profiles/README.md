# Embedded sRGB profile

`sRGB-v2-magic.icc` is the unmodified 736-byte sRGB ICC v2.1 profile,
with a matrix and 182-point curve, from
[Compact ICC Profiles](https://github.com/saucecontrol/Compact-ICC-Profiles),
by Clinton Ingram (saucecontrol). It is dedicated to the public domain under
CC0 1.0 Universal; the upstream licence is included as `LICENSE-CC0`.
This asset is separate from rawsmith's MIT / Apache-2.0 code licence.

Pinned upstream revision: `bdd84663061bc4ae95ca70decff54f581e27f702`.

- [Profile source](https://github.com/saucecontrol/Compact-ICC-Profiles/blob/bdd84663061bc4ae95ca70decff54f581e27f702/profiles/sRGB-v2-magic.icc)
- [Licence source](https://github.com/saucecontrol/Compact-ICC-Profiles/blob/bdd84663061bc4ae95ca70decff54f581e27f702/license)
- SHA-256: `af4efe28f6d311799865f325ba39184a2e978b113ca74124d60af7de22b105f4`

The SHA-256 was verified against the profile bytes at the pinned revision;
the Git blob SHA-1 is `b29f729dbff1d6eae55aa62418f79b42b1fce9e8`.
The ICC header version bytes (offsets 8..12) are `02 10 00 00` (v2.1).
The ICC `desc` tag contains the ASCII description `sRGB` (including a trailing
NUL in the tag). This lets viewers such as macOS identify the profile as sRGB;
the smaller micro variant describes itself as `uRGB`. The magic variant uses
the upstream 182-point curve for greater accuracy.

The JPEG encoder embeds these bytes in an ICC APP2 segment. This declares
the already encoded sRGB pixels; it does not convert or change samples.
PNG uses the standard sRGB chunk instead. Neither carries camera metadata.
