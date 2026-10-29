# rawsmith

[![CI](https://github.com/Lewin671/rawsmith/actions/workflows/ci.yml/badge.svg)](https://github.com/Lewin671/rawsmith/actions/workflows/ci.yml)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#licence)

**Let Claude Code or Codex edit your RAW photos.**

rawsmith is a command-line RAW developer built for AI assistants, plus a
skill that teaches the assistant how to edit. Ask it to edit a photo and it
works like a photo editor: it looks at the picture, decides what it should
be, crops, sets light and colour, checks the result against the camera's
JPEG and exports. You don't need to describe a look; if you have one in
mind, say so and it comes first.

- **Local.** Photos are developed on your machine; the RAW is never modified.
- **Reproducible.** Each edit is a small JSON recipe; the same recipe and
  binary give the same output, bit for bit.
- **Made for agents.** Every command returns JSON, and renders report tone,
  clipping and colour statistics, so the assistant can check its work by
  numbers as well as by eye.

![Garden pond: camera JPEG on the left, assistant edit on the right](docs/images/pond-comparison.jpg)

*Left: the camera's JPEG. Right: the assistant's edit, asked only to “edit
this photo”. [More examples](#examples).*

## Quick start

Install the CLI and the skill for Claude Code and Codex (macOS or Linux,
no Rust needed):

```sh
curl -fsSL https://github.com/Lewin671/rawsmith/releases/latest/download/install.sh | sh
```

Then ask your assistant:

> Use rawsmith to edit ~/Pictures/DSC0001.ARW.

Add a direction if you like: *“keep it dark and quiet”*, *“the foreground
is too dark”*. See [installation options](docs/install.md) for PATH setup,
single-agent installs and building from source.

**Supported cameras:** Sony A7 IV (ILCE-7M4) Bayer ARW: uncompressed,
lossy-compressed, and lossless-compressed full-frame L / APS-C M.
Lossless full-frame M/S and APS-C S are not supported.
[Format details](docs/formats.md) · [Add a camera](CONTRIBUTING.md)

## How it works

```text
RAW file + recipe.json  ──rawsmith──▶  JPEG / 16-bit PNG + JSON report
```

The assistant edits by changing the recipe, rendering a preview, looking at
it and repeating. A typical session:

```sh
rawsmith inspect photo.ARW                      # camera settings, exposure, clipping
rawsmith auto photo.ARW -o start.jpg            # measured starting recipe + preview
rawsmith edit photo.ARW --set shadows=40 \
  --set temperature=5900 -o edit.jpg            # change and save settings, render a preview
rawsmith compare photo.ARW -o compare.jpg       # camera JPEG beside the edit
rawsmith render photo.ARW --region 0.4,0.3,0.6,0.5 -o detail.jpg   # 100% detail check
rawsmith render photo.ARW -o final.jpg          # full-resolution export
```

| Command | What it does |
|---|---|
| `inspect` | Describe the RAW: compression, camera settings, white balance, sensor exposure |
| `schema` | List every recipe parameter with its range, default and meaning |
| `auto` | Analyse the scene and save an automatic starting recipe |
| `edit` | Change recipe parameters, save the recipe, optionally render a preview |
| `render` | Render from the recipe (or try settings with `--set` without saving) and report image statistics |
| `compare` | Show camera and recipe versions side by side and note what each gave up |
| `extract-jpeg` | Copy the camera's embedded JPEG unchanged |

The recipe is saved next to the RAW as `<raw>.rawsmith.json` and holds
only what differs from the defaults, for example:

```json
{
  "crop": { "left": 0.06, "top": 0.08, "right": 0.96, "bottom": 0.98 },
  "shadows": 45, "highlights": -55, "contrast": 12,
  "temperature": 5900, "tint": 28, "vibrance": 25,
  "hsl": { "orange": { "saturation": 30 } },
  "vignette": -25
}
```

Recipes cover exposure, white balance, tone and curves, clarity, dehaze,
HSL, colour grading, crop and rotation, noise reduction, sharpening,
defringing, vignette, and local edits with graduated filters and
linear/radial masks. `rawsmith schema` lists them all.

[Editing skill](skill/rawsmith/SKILL.md) · [CLI and recipe reference](docs/cli.md) · [Developing pipeline](docs/pipeline.md)

## Examples

Camera JPEG on the left, assistant edit on the right; each request was only
*“edit this photo”*. Sony A7 IV photographs, reproduced with permission.

*Rime on pines: the camera rendered the frost blue-grey; the edit makes it
white against a deeper sky.*

![Rime: camera JPEG on the left, assistant edit on the right](docs/images/rime-comparison.jpg)

*Ginkgo at dusk (ISO 4000): the surroundings go cool and quiet so the gold
leaves carry the picture.*

![Ginkgo: camera JPEG on the left, assistant edit on the right](docs/images/ginkgo-comparison.jpg)

[Recipes for these edits](docs/recipes) · [Photo permissions](docs/images/README.md)

## Results

Ten Sony A7 IV photos were edited by an assistant following the
skill, with no direction beyond *“edit these photos”*, then judged blind
against other renderings. Counts are wins – losses – ties.

| Comparison | All 10 photos | 6 never used in development |
|---|---|---|
| Assistant edit vs camera JPEG | 10 – 0 – 0 | 6 – 0 – 0 |
| Assistant edit vs macOS RAW converter | 10 – 0 – 0 | 6 – 0 – 0 |
| Assistant edit vs `auto` | 8 – 0 – 2 | 5 – 0 – 1 |
| `auto` vs camera JPEG | 6 – 0 – 4 | 2 – 0 – 4 |

The sample is small, and the judge (Codex `gpt-6.1-sol`, both image orders,
with control pairs) was the same model as the editor. The main gain comes
from the assistant's choices of crop, brightness and palette; the engine
alone is roughly level with the camera.
[Method, limits and full results](docs/benchmark.md).

The decoder matches LibRaw sample for sample on all 20 test files. On an
8-core Intel i7-10700, a 33 MP photo's first preview takes 3–4 s, and
later previews in the same session about 1.3 s.
[Performance details](docs/performance.md).

## Limits

- Only Sony A7 IV Bayer ARW for now; other cameras need a colour matrix and
  a test file ([CONTRIBUTING.md](CONTRIBUTING.md)).
- No brushes, subject masks, local white balance, or spot and dust removal.
- Output is sRGB JPEG or 16-bit PNG, without camera metadata.
- Known weak spots: a blown sun pulled far down can show a hard-edged disc;
  fine branches against a blown sky can keep faint coloured specks; at high
  ISO, texture weaker than the noise is smoothed away.

## Privacy

rawsmith works offline. Images from `render`, `edit`, `auto` and `compare`
contain no EXIF, GPS or serial numbers; `extract-jpeg` keeps the camera
JPEG's metadata. If your assistant runs in the cloud, the previews it looks
at are sent to its provider, like any image you show it.

The optional `rawsmith-select` helper, installed alongside, compares
candidate edits with Codex and only runs with explicit cloud consent.

## Documentation

| | |
|---|---|
| [docs/install.md](docs/install.md) | Install options, skill locations, building from source |
| [docs/cli.md](docs/cli.md) | Commands, recipe parameters, reports, error handling |
| [skill/rawsmith/SKILL.md](skill/rawsmith/SKILL.md) | The editing workflow the assistant follows |
| [docs/pipeline.md](docs/pipeline.md) | Developing stages, from decoding to sharpening |
| [docs/formats.md](docs/formats.md) | Supported ARW formats |
| [docs/benchmark.md](docs/benchmark.md) | Quality measurements and photo judging |
| [docs/performance.md](docs/performance.md) | Speed and memory measurements |
| [CHANGELOG.md](CHANGELOG.md) | Release notes |
| [CONTRIBUTING.md](CONTRIBUTING.md) | Building, testing, adding a camera |

## Licence

Code: [Apache License 2.0](LICENSE-APACHE) or [MIT](LICENSE-MIT), at your
option; contributions use the same terms. The example photographs have
[separate permissions](docs/images/README.md). The embedded sRGB profile is
CC0 ([source](assets/profiles/README.md)). The camera colour matrix is
sensor characterisation data published in Adobe's DNG Converter and
reproduced by open-source RAW software.
