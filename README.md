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

## Before and after

The camera's embedded JPEG is on the left; the RAW developed with rawsmith
is on the right. These are **AI-edited results**: `auto` provided the
starting point, then an assistant refined the recipe while checking
previews. The edits include cropping; both sides show their complete frame.

**Snowman — brighten underexposed snow and bring out the subject's texture.**

![Snowman: camera JPEG on the left, AI-edited rawsmith result on the right](docs/images/snowman-comparison.jpg)

**Sunset — deepen the warm sky, preserve the mountain layers and tighten the framing.**

![Sunset: camera JPEG on the left, AI-edited rawsmith result on the right](docs/images/sunset-comparison.jpg)

**Snow mountain — soften the blue cast in shade and lift the foreground while keeping the warm summit light.**

![Snow mountain: camera JPEG on the left, AI-edited rawsmith result on the right](docs/images/snow-mountain-comparison.jpg)

Sony A7 IV photographs, reproduced with permission. The exact recipes are
in [docs/recipes](docs/recipes); photo permissions are described in
[docs/images/README.md](docs/images/README.md).

<details>
<summary>One-command auto example (CC0 sample)</summary>

| Camera JPEG | `rawsmith auto` |
|---|---|
| ![The camera's own rendering](docs/images/demo-camera.jpg) | ![rawsmith auto rendering](docs/images/demo-rawsmith.jpg) |

A CC0 sample from [raw.pixls.us](https://raw.pixls.us) (Sony A7 IV), developed with one command.

</details>

## Install

Install the latest prebuilt release and the skill for both Claude Code and
Codex:

```sh
curl -fsSL https://github.com/Lewin671/rawsmith/releases/latest/download/install.sh | sh
```

Supports macOS 11+ (Apple Silicon and Intel) and Linux (ARM64 and x86_64;
static musl binaries). Requires `curl`, `tar` and `sha256sum` or `shasum`.
No Rust toolchain or `sudo` is needed. Downloads are pinned to one release
and checked against its SHA-256 manifest before installation.

The binary goes in `~/.local/bin`. If that directory is not already on
your PATH, add it to your shell configuration and reload your shell:

```sh
export PATH="$HOME/.local/bin:$PATH"
```

The installer prints its destinations and does not edit shell profiles.
Choose one agent, skip skills, pin a version, or change the binary directory
by passing options to `sh -s --`:

```sh
curl -fsSL https://github.com/Lewin671/rawsmith/releases/latest/download/install.sh | sh -s -- --skills codex
# Other options: --skills claude | --skills none | --version 0.1.0 | --bin-dir "$HOME/bin"
```

Re-running updates installer-managed skills. Locally customized skills
are preserved; `--force` saves a backup before replacing them. See
[release downloads](https://github.com/Lewin671/rawsmith/releases) or
`install.sh --help` for all options.

To build from source instead:

```sh
git clone https://github.com/Lewin671/rawsmith
cd rawsmith
cargo install --locked --path .
```

Source builds require a recent stable Rust toolchain. No other dependencies.

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

An explicit `render --recipe FILE` requires that file to exist, so a
misspelled path cannot silently export an unedited picture. `edit` and
`auto` can create a new recipe at that path. Without `--recipe`, a missing
sidecar means the default recipe. `--size` must be at least 1 pixel and
`--quality` is 1–100; invalid values are reported as JSON errors.

`rawsmith schema` lists every parameter with its range and meaning:
white balance (`temperature`, `tint`), tone (`exposure`, `contrast`,
`highlights`, `shadows`, `whites`, `blacks`, `clarity`, `dehaze`, `curve`),
colour (`vibrance`, `saturation`, eight `hsl` bands, shadow/highlight
`grade`), detail (`sharpen`, `denoise`), and geometry (`rotate`, `crop`,
`lens_corrections`).

## Use it from an AI assistant

[`skill/rawsmith/SKILL.md`](skill/rawsmith/SKILL.md) teaches an assistant
the editing loop: inspect, start from `auto`, look at the preview, fix the
biggest problem first, compare, check detail, export. The one-command
installer installs the same skill for both agents:

| Agent | User skill directory | Project skill directory |
|---|---|---|
| [Claude Code](https://code.claude.com/docs/en/skills#choose-where-skills-load) | `~/.claude/skills/rawsmith/` | `.claude/skills/rawsmith/` |
| [Codex](https://learn.chatgpt.com/docs/build-skills#where-codex-loads-local-skills) | `~/.agents/skills/rawsmith/` | `.agents/skills/rawsmith/` |

Claude Code's user directory follows `CLAUDE_CONFIG_DIR` when configured.
For a project-scoped installation, pass `--claude-skills-dir .claude/skills`
and/or `--codex-skills-dir .agents/skills`. Older Codex installations that
use `~/.codex/skills` can select it explicitly with
`--codex-skills-dir "$HOME/.codex/skills"`; the default follows current
Codex documentation and avoids installing duplicate copies.

For a source checkout, install the skill manually:

```sh
mkdir -p ~/.claude/skills ~/.agents/skills
cp -r skill/rawsmith ~/.claude/skills/
cp -r skill/rawsmith ~/.agents/skills/
```

If the skill does not appear, reload skills or restart your agent. Then
ask: *"Edit ~/Pictures/DSC0001.ARW — the foreground is too dark."*

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

rawsmith runs locally and never uses the network. Pictures developed by
`render`, `edit` and `auto` carry no EXIF, GPS position or serial numbers.
`extract-jpeg` copies the camera's embedded JPEG unchanged, including any
metadata it contains; check or remove that metadata before sharing it.
If you let a cloud-hosted assistant
drive it, the previews it looks at are sent to that assistant's provider,
like any other image you show it.

## Licence

Licensed under either of [Apache License 2.0](LICENSE-APACHE) or
[MIT licence](LICENSE-MIT) at your option. Contributions are accepted under
the same terms.

The camera colour matrix is sensor characterisation data as published in
Adobe's DNG Converter and reproduced by open-source raw software. The
one-command demo photo is a CC0 sample from [raw.pixls.us](https://raw.pixls.us). Other
photograph permissions are listed in [docs/images/README.md](docs/images/README.md).
