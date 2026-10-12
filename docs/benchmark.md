# Benchmark

rawsmith is measured three ways. Each answers a different question, and
none of them substitutes for the others.

| Benchmark | Question | Needs photos? |
|---|---|---|
| Decoder cross-check | Are the sensor samples read correctly? | Any ARW files |
| `examples/quality.rs` | Are the algorithms correct and better than naive baselines, against known answers? | No (synthetic) |
| `bench/photos.py` | Do real photos come out better than the camera's own rendering and the system RAW converter, and does AI editing help? | Your own RAW files |

Numbers below are from version RAWSMITH_VERSION on an Intel i7-10700
(8 cores / 16 threads), macOS, release build.

## 1. Decoder cross-check

`examples/dump.rs` writes the decoded sensor samples; they are compared with
LibRaw's (`rawpy`) sample for sample, together with black level, white
level and as-shot white balance.

| Files | Result |
|---|---|
| 15 uncompressed 14-bit ARW (Sony A7 IV, private test set) | identical |
| 2 uncompressed + 1 lossy-compressed ARW (A7 IV, CC0, [raw.pixls.us](https://raw.pixls.us)) | identical |

## 2. Ground-truth quality (`cargo run --release --example quality`)

Synthetic scenes with a known correct answer. Reproducible by anyone;
`--check` turns the floors into a regression test that CI runs.

QUALITY_TABLE

- **Demosaic**: PSNR of the reconstructed image against the original, over
  four scenes (zone plate up to 0.8 of the sensor's resolution limit, thin
  coloured lines, soft coloured shapes, fractal texture). Baselines:
  bilinear and Malvar–He–Cutler, both implemented in the benchmark.
- **Highlight reconstruction**: a warm gradient whose green and red
  channels saturate; mean relative error over pixels where one or two
  channels were lost, against simply clipping.
- **Colour accuracy**: 24 known colours written into a synthetic RAW and
  developed through the full path (file → decode → white balance →
  demosaic → camera matrix). This verifies the colour pipeline is
  self-consistent; it does not measure how accurate the camera's matrix is
  for real scenes.
- **Noise reduction**: signal-dependent noise on flat areas with an edge;
  chroma noise removed, edge contrast kept.
- **Sharpening**: gain in edge slope and overshoot on a soft step edge.

## 3. Real photos, judged blind (`bench/photos.py`)

### Method

Every RAW file is rendered several ways at 1400 px:

| Variant | What it is |
|---|---|
| `camera` | the JPEG the camera embedded in the RAW: the manufacturer's rendering |
| `system` | macOS's RAW converter (`sips`), converted to sRGB |
| `neutral` | rawsmith, default recipe |
| `auto` | rawsmith after `rawsmith auto` |
| `ai` | rawsmith with a recipe made by an AI assistant following [the skill](../skill/rawsmith/SKILL.md), starting from `auto` |

An LLM judge sees two renderings of one photo, labelled only A and B, and
picks the better finished photograph or declares a tie. To keep that
honest:

- **Blind**: file names are random task numbers; the prompt says nothing
  about what is being compared.
- **Both orders**: every pair is judged twice with A and B swapped. A
  photo counts as a win only if the verdicts agree; disagreement is a tie.
- **The photo is the unit**: one outcome per photo per comparison, however
  many times it was judged.
- **Judge checks**: the judge is also shown identical pairs (should be
  ties) and deliberately spoiled renderings — 2.5 stops underexposed,
  strong colour cast, oversaturated, flat — which it must reject. Its
  accuracy on those, and how often it keeps its verdict when the order is
  swapped, are reported next to the results.
- **Numbers only**: the report contains no images, file names or paths.

What the comparisons mean:

- `neutral vs camera`, `auto vs camera`, `auto vs system`: the engine
  against existing renderings.
- `auto vs neutral`: what automatic adjustment adds.
- `ai vs auto`: what an assistant's editing loop adds on top — the reason
  the tool exists.

### Results

PHOTO_TABLE

### Limits

- The private set is 15 photos from one camera and one photographer:
  dusk and sunsets, hazy lake views, snow and rime in fog and sun. The
  intervals are wide, and the default look and `auto` were developed while
  looking at these same photos, so this is a development-set result, not a
  held-out one.
- An LLM judge measures plausibility of taste, not truth. It is good at
  rejecting obvious faults (see its control accuracy) and reasonably
  consistent, but it is one opinion.
- `camera` is a resized JPEG, so it has been compressed twice.

### Run it on your own photos

```sh
cargo build --release
bench/photos.py render --raws ~/Pictures/raw --work ~/rawsmith-bench
bench/photos.py pairs  --work ~/rawsmith-bench
bench/photos.py judge  --work ~/rawsmith-bench     # default judge: `claude -p`
bench/photos.py report --work ~/rawsmith-bench
```

`--judge-cmd` takes any command that reads the prompt on standard input,
can open the two image paths named in it, and prints a JSON verdict. The
work directory must be outside the repository.
