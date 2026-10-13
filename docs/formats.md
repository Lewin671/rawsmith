# Supported RAW formats

Only Sony A7 IV (ILCE-7M4) Bayer files have camera support.

| Encoding | Full-frame modes | APS-C modes | `inspect.compression` |
|---|---|---|---|
| Uncompressed | Supported | Supported | `uncompressed` |
| Sony lossy ARW2 | Supported | Supported | `lossy-compressed` |
| Lossless Huffman JPEG, Bayer | L supported | M supported | `lossless-compressed` |
| Lossless Huffman JPEG, YCbCr | M/S unsupported | S unsupported | JSON `unsupported` error |

No separately labelled APS-C L lossless sample was present in the
raw.pixls.us camera listing when checked on 2026-10-13. Other cameras and
unverified Sony component layouts are outside the supported set.

## Lossless Bayer layout

The two verified files use TIFF compression 7 and photometric 32803.
TileWidth/TileLength (0x0142/0x0143) are 512×512; TileOffsets and
TileByteCounts (0x0144/0x0145) describe independent JPEG streams. Their
arrays are in row-major image order, while payloads in these files are
stored column-major. Strip tags also exist, but do not describe the tiles.

Each tile has SOI, SOF3, DHT, SOS, entropy-coded differences and EOI. SOF3
specifies 14-bit precision, 256×256 dimensions, four components (IDs 1–4),
each with sampling 1×1. SOS uses predictor 1 (left neighbour), DC table 0,
and point transform 0. Components 1/2 become the top row of a 2×2 Bayer
block and 3/4 the bottom row; sample values are used without a tone curve.
Each component's first prediction is 8192, the first row predicts from the left and
the first column of subsequent rows from above. TIFF payloads may contain
up to 15 bytes of 0xff alignment fill following EOI.

The decoder also implements predictors 2–7, multiple DC tables, point
transforms and 16-bit modulo difference category 16, checked with synthetic
files. It accepts only one complete four-component scan with equal sampling,
without restart intervals, arithmetic coding, differential/hierarchical
frames or subsampling. These broader JPEG modes have no real-file acceptance.

Allocation/work bounds: sensor edges ≤8192, ≤40 million sensor samples,
≤4096 non-overlapping tiles, ≤50 million padded tile samples, tile edges
≤512, ≤2 MB per tile and ≤4096 bytes of header per tile. Offset/count
arrays must have exactly the expected tile count. Missing/truncated data,
bad Huffman codes/tables, incorrect geometry and absent EOI return
`format`; unimplemented encodings/layouts return `unsupported`.

## Reduced-size modes

The full-frame M sample uses photometric 6, three 15-bit TIFF components
and a 16-bit SOF3 frame with 2×2, 1×1, 1×1 sampling (YCbCr 4:2:0).
Full-frame S and APS-C S use 2×1, 1×1, 1×1 (YCbCr 4:2:2). CFA tags are
all 255. These are colour images rather than a Bayer sensor mosaic; they
cannot be fed to the existing Bayer developing pipeline. They are explicitly
rejected rather than interpreted as lossy ARW2 or a malformed CFA file.
The APS-C M sample, despite its size label, retains the four-plane Bayer
layout and decodes exactly. Size labels alone do not identify the encoding.

## References and implementation provenance

- [ITU-T T.81 / ISO/IEC 10918-1](https://www.w3.org/Graphics/JPEG/itu-t81.pdf),
  Annex B (markers/frames/scans), Annex C (canonical Huffman tables), and
  Annex H (lossless prediction, boundary conditions, modulo differences,
  category 16 and point transform).
- [Description of Sony RAW formats, Laurent Clévy](https://github.com/lclevy/sony_raw/blob/main/readme.md),
  prose description identifying ARW 4.0 and lossless JPEG. Its implementation
  links were not used as code sources.
- The five [raw.pixls.us](https://raw.pixls.us) CC0 files and their TIFF/JPEG
  headers establish Sony's tile/component arrangement and size specifics;
  [verification counts](benchmark.md#1-decoder-cross-check).

The Rust codec and synthetic encoder were independently written from the
standard and observed file structure. No LibRaw, RawTherapee, darktable or
rawspeed source was copied or ported. LibRaw through rawpy is an external
verification oracle, never a runtime dependency. Photographs, dumps,
virtualenv and renders remain outside the repository.
