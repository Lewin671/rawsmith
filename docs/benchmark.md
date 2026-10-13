# Benchmark

rawsmith is measured three ways. Each answers a different question, and
none of them substitutes for the others.

| Benchmark | Question | Needs photos? |
|---|---|---|
| Decoder cross-check | Are the sensor samples read correctly? | Any ARW files |
| `examples/quality.rs` | Are the algorithms correct and better than naive baselines, against known answers? | No (synthetic) |
| `bench/photos.py` | Do real photos come out better than the camera's own rendering and the system RAW converter, and does AI editing help? | Your own RAW files |

Numbers below are from version 0.1.0 on an Intel i7-10700
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

| Measure | rawsmith | Baseline | Floor checked in CI |
|---|---|---|---|
| Demosaic PSNR, mean of four scenes | 39.4 dB | bilinear 33.2 dB, Malvar–He–Cutler 37.1 dB | +4 dB over bilinear, +1 dB over MHC |
| Demosaic false colour, zone plate (lower is better) | 0.025 | bilinear 0.084, MHC 0.030 | — |
| Highlight reconstruction, mean relative error | 1.1% | 10.3% when clipping | below 3% |
| Colour accuracy, 24 patches (ΔE, Oklab × 100) | mean 0.014, max 0.036 | — | mean below 0.5, max below 1 |
| Chroma noise removed / edge contrast kept | 74% / 108% | — | at least 25% / 95% |
| Sharpening overshoot, share of the edge step | 1.7% | — | below 2% |
| Tone curve monotone | yes | — | yes |

Per scene, demosaic PSNR in dB (bilinear / MHC / rawsmith): zone plate
19.6 / 25.7 / 28.6, coloured lines 26.8 / 26.1 / 27.6, soft shapes
41.8 / 45.1 / 48.2, fractal texture 44.8 / 51.6 / 53.1.

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
  photo counts as a win when the two verdicts net out in its favour;
  opposite verdicts cancel to a tie.
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

15 photos (the private set described under Limits), judge: Claude through
`claude -p`, one judge, 258 judgements, every pair in both orders.

| Comparison | Photos | Wins | Losses | Ties | Win rate of decided | 95% interval |
|---|---|---|---|---|---|---|
| `neutral` vs `camera` | 15 | 10 | 0 | 5 | 100% | 72–100% |
| `auto` vs `camera` | 15 | 15 | 0 | 0 | 100% | 80–100% |
| `auto` vs `system` | 15 | 15 | 0 | 0 | 100% | 80–100% |
| `auto` vs `neutral` | 15 | 13 | 0 | 2 | 100% | 77–100% |
| `ai` vs `auto` | 15 | 12 | 1 | 2 | 92% | 67–99% |
| `ai` vs `camera` | 15 | 15 | 0 | 0 | 100% | 80–100% |
| `ai` vs `system` | 15 | 15 | 0 | 0 | 100% | 80–100% |

A win is a photo whose two verdicts net out in the candidate's favour.
The interval is a Wilson interval on wins among decided photos.

Judge checks for this run:

| Check | Result |
|---|---|
| Spoiled controls rejected | 40 of 40 |
| Identical pairs called a tie | 8 of 8 |
| Same verdict after swapping A and B | 92% of pairs |
| Position preference | A 122, B 118, tie 18 |

Speed on the same machine, mean of the 15 files (33 MP each): a 1400 px
preview takes 0.39 s; a full-resolution JPEG export takes 4.3 s,
including encoding and writing. `rawsmith auto` with a preview takes 1.05 s
and macOS `sips` 1.30 s for a picture of the same size.

The `ai` recipes came from five independent assistant sessions (three
photos each) that were given only the skill file and the CLI, started
from `auto`, and used two to five rounds per photo.

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
