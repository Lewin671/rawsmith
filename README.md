# rawsmith

[![CI](https://github.com/Lewin671/rawsmith/actions/workflows/ci.yml/badge.svg)](https://github.com/Lewin671/rawsmith/actions/workflows/ci.yml)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#licence)

A RAW photo developer built for AI assistants. `rawsmith` decodes a camera
RAW file and develops it into a finished picture from a small JSON recipe —
and it is designed so an assistant can do the editing: read the photo,
change parameters, look at the result, and repeat until it is right.

- **Own engine, written in Rust.** RAW decoding, demosaicing, highlight
  reconstruction, lens corrections, colour, tone mapping, local contrast,
  dehaze, noise reduction and sharpening are implemented here. No LibRaw,
  RawTherapee or Lightroom underneath.
- **Made for an edit loop.** Every command prints JSON. Previews render in
  under half a second. Each render reports numbers an assistant can reason
  about (tone percentiles, clipping, colour cast, a 3×3 brightness grid)
  next to the image it can look at.
- **Non-destructive and reproducible.** The RAW is only ever read. An edit
  is a short, human-readable recipe file next to it; the same recipe always
  produces the same picture.
- **Measured.** Decoded samples are verified against LibRaw; algorithms are
  benchmarked against ground truth; real-photo results are judged blind
  against the camera's own JPEG and the system RAW converter. See
  [Results](#results).

| Camera JPEG | `rawsmith auto` |
|---|---|
| ![The camera's own rendering](docs/images/demo-camera.jpg) | ![rawsmith auto rendering](docs/images/demo-rawsmith.jpg) |

*A CC0 sample from [raw.pixls.us](https://raw.pixls.us) (Sony A7 IV), developed with one command.*

## Install

```sh
git clone https://github.com/Lewin671/rawsmith
cd rawsmith
cargo install --path .
```

Requires a recent stable Rust toolchain. No other dependencies.

## Use

```sh
# What is in this file?
rawsmith inspect photo.ARW

# A good starting point, explained, plus a preview to look at
rawsmith auto photo.ARW -o preview.jpg --size 1400

# Refine: set values, or nudge them with += / -=
rawsmith edit photo.ARW --set shadows=35 --set highlights-=20 -o preview.jpg --size 1400

# Check fine detail at 100%
rawsmith render photo.ARW --region 0.4,0.4,0.6,0.6 -o detail.jpg

# Export at full resolution (.jpg, or .png for 16-bit)
rawsmith render photo.ARW -o final.jpg
```

The edit is stored in `photo.ARW.rawsmith.json`, containing only what
differs from the defaults:

```json
{
  "exposure": 0.35,
  "highlights": -40.0,
  "shadows": 35.0,
  "vibrance": 20.0,
  "hsl": { "blue": { "saturation": -15.0 } },
  "rawsmith": "0.1.0"
}
```

`rawsmith schema` lists every parameter with its range and meaning:
white balance (`temperature`, `tint`), tone (`exposure`, `contrast`,
`highlights`, `shadows`, `whites`, `blacks`, `clarity`, `dehaze`, `curve`),
colour (`vibrance`, `saturation`, eight `hsl` bands, shadow/highlight
`grade`), detail (`sharpen`, `denoise`), and geometry (`rotate`, `crop`,
`lens_corrections`).

## Use it from an AI assistant

[`skill/rawsmith/SKILL.md`](skill/rawsmith/SKILL.md) teaches an assistant
the editing loop: inspect, start from `auto`, look at the preview, fix the
biggest problem first, compare, check detail, export. For Claude Code:

```sh
cp -r skill/rawsmith ~/.claude/skills/
```

Then ask: *"Edit ~/Pictures/DSC0001.ARW — the foreground is too dark."*

## Results

**Decoding.** Sensor samples are identical to LibRaw's on all 18 test
files (15 uncompressed, 2 more uncompressed and 1 lossy-compressed from
raw.pixls.us).

**Algorithms, against ground truth** (`cargo run --release --example quality`):

| Measure | rawsmith | Baseline |
|---|---|---|
| Demosaic PSNR, mean of four scenes | 39.4 dB | bilinear 33.2 dB, Malvar–He–Cutler 37.1 dB |
| Highlight reconstruction error | 1.1% | 10.3% when clipping |
| Chroma noise removed, edge contrast kept | 74%, 108% | — |
| Sharpening overshoot | 1.7% of the edge step | — |

**Real photos, judged blind.** 15 photos; each pair is shown without
labels, in both orders, to two LLM judges from different vendors. Wins –
losses – ties per photo:

| Comparison | Claude | GPT-6 Luna | What it shows |
|---|---|---|---|
| `auto` vs the camera's JPEG | 15 – 0 – 0 | 12 – 0 – 3 | the engine and its automatic adjustment |
| `auto` vs macOS's RAW converter | 15 – 0 – 0 | 12 – 0 – 3 | |
| default recipe vs the camera's JPEG | 10 – 0 – 5 | 5 – 0 – 10 | the engine alone |
| AI-edited vs `auto` | 12 – 1 – 2 | 6 – 0 – 9 | what an assistant's edit loop adds |

Both judges called all identical pairs a tie and rejected deliberately
spoiled controls (40 of 40 and 36 of 40). The second judge is noisier —
it keeps its verdict in 75% of pairs when the order is swapped, against
92% — which shows up as more ties. This is a small development set from
one camera: read it as evidence that the approach works, not as a ranking
of raw developers.

**Speed** (33 MP files, 8-core desktop): 0.4 s for a 1400 px preview,
4.3 s for a full-resolution JPEG.

Method, full tables and how to run the benchmark on your own photos:
[docs/benchmark.md](docs/benchmark.md).

## Supported files

| Camera | Formats |
|---|---|
| Sony A7 IV (ILCE-7M4) | ARW, uncompressed and lossy-compressed |

Lossless-compressed ARW is not supported yet. Other cameras need a colour
matrix and a test file — see [CONTRIBUTING.md](CONTRIBUTING.md). Output is
sRGB JPEG or 16-bit PNG, without camera metadata.

## How it works

[docs/pipeline.md](docs/pipeline.md) walks through the developing pipeline
stage by stage.

## Privacy

rawsmith runs locally and never uses the network. Output files carry no
EXIF, GPS position or serial numbers. If you let a cloud-hosted assistant
drive it, the previews it looks at are sent to that assistant's provider,
like any other image you show it.

## Licence

Licensed under either of [Apache License 2.0](LICENSE-APACHE) or
[MIT licence](LICENSE-MIT) at your option. Contributions are accepted under
the same terms.

The camera colour matrix is sensor characterisation data as published in
Adobe's DNG Converter and reproduced by open-source raw software. The demo
photo is a CC0 sample from [raw.pixls.us](https://raw.pixls.us).
