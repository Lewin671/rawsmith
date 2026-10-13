# Benchmark

rawsmith is measured three ways. Each answers a different question, and
none of them substitutes for the others.

| Benchmark | Question | Needs photos? |
|---|---|---|
| Decoder cross-check | Are the sensor samples read correctly? | Any ARW files |
| `examples/quality.rs` | Are the algorithms correct and better than naive baselines, against known answers? | No (synthetic) |
| `bench/photos.py` | Do real photos come out better than the camera's own rendering and the system RAW converter, and does AI editing help? | Your own RAW files |

Decoder and synthetic measurements below are from version 0.1.0. Real-photo
results distinguish the sealed evaluation, validation before and after fixes,
and historical development runs. Timings were measured on an Intel i7-10700
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
| `camera` | the embedded JPEG rendered upright through `rawsmith compare --panel camera`; established sRGB or Adobe RGB converted to sRGB |
| `system` | macOS's RAW converter (`sips`), converted to sRGB |
| `neutral` | rawsmith, default recipe |
| `auto` | rawsmith after `rawsmith auto` |
| `ai` | rawsmith with a recipe made by an AI assistant following [the skill](../skill/rawsmith/SKILL.md), starting from `auto` |

Current rawsmith previews run the full-resolution pipeline, including noise
reduction, tone and sharpening, then resize the finished sRGB pixels last.
They take about as long as an export; the earlier approximate preview path
skipped noise reduction and no longer exists. The judge sees resized images,
so these comparisons alone cannot establish full-resolution detail quality.

The `camera` variant works without macOS. `render` calls
`rawsmith compare RAW --panel camera -o camera.jpg --size 1400` and reads its
colour report. Unknown, conflicting, uncalibrated or unverified colour skips
that photo's `camera` variant with a message; camera rendering failures also
skip it. No pairs against `camera` are generated for skipped photos.
The manifest records each photo's camera status, colour declaration and
conversion (when available), plus `camera_color_space` counts of `srgb`,
`adobe_rgb_converted` and `skipped`. The JSON and text report include those
counts; older manifests report them as unknown (`null`), not zero.
Only `system` still requires macOS `sips`.

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
- **Metrics and provenance only**: the report contains no images, photo file
  names or private paths. Judge command lines are published as hashes.

Judge results are cached under `--name` in a versioned verdict file. A file
configuration fingerprint covers the exact command, explicitly selected model
and reasoning effort, prompt template and harness format (plus the built-in
adapter's source hash).
Changing any of these under an existing name refuses the run and leaves the
file unchanged: choose a new `--name`, or archive the old file yourself first.
Legacy verdict files without fingerprints or recorded effort are unverifiable;
`judge` refuses to reuse or overwrite them, and `report` labels and excludes them.

Each verdict also fingerprints the exact formatted prompt, both blind image
byte hashes, and the task including its photo grouping and A/B assignment.
Resume reuses only matching successful verdicts and re-judges affected tasks;
unchanged tasks survive image replacement or task regeneration. Parallel
results are saved atomically as they finish; interrupted runs retain that work.
Failed commands (even if they print JSON) are retried and never counted as
evidence; a run with failures exits
non-zero. `report` rechecks current tasks and image bytes, excludes stale,
failed, missing-image and removed-task verdicts, and counts incomplete pairs
separately. Changing the built-in adapter also invalidates its old evidence.

The local verdict file records the literal command, model, reasoning effort,
prompt SHA-256, harness format/version, timestamps for each run and verdict,
and each run's task-set identity: seed, task count, task digest and blind-image digest. The JSON
report includes this evaluation provenance, validation counts and the current
task-set identity. To keep reports publishable, it replaces literal commands
with their SHA-256 and a logical adapter label (`bench/judge-codex.py` or
`custom`); even custom command arguments can contain private paths. The
manifest and report also record the binary's `rawsmith --version` output and
the checkout's Git commit when available. Older manifests have unknown version,
commit or seed fields (`null`); those facts are not reconstructed.

What the comparisons mean:

- `neutral vs camera`, `auto vs camera`, `auto vs system`: the engine
  against existing renderings.
- `auto vs neutral`: what automatic adjustment adds.
- `ai vs auto`: what an assistant's editing loop adds on top — the reason
  the tool exists.

### Sealed results: 10 photos, evaluated once after fixes

The sealed set contains 10 further Sony A7 IV photos from the same selection
as the validation set: 3 high ISO, 3 backlit, 3 saturated sky and 1 snow.
It was not looked at until all fixes were merged, then edited and judged
once. All camera JPEGs in both sets were Adobe RGB and were converted to
sRGB by the harness.

For both the sealed and validation evaluations, the blind judge was Codex
`gpt-6.1-sol` at **low** reasoning effort, using both image orders, spoiled
controls and identical pairs. The `ai` recipes were made by Codex
`gpt-6.1-sol` at **high** effort following the editing skill. Editor and
judge are the same model at different effort levels, which may favour `ai`.
Cells are wins – losses – ties for the first-named variant, one outcome per
photo.

| Comparison | Sealed after fixes (10 photos) |
|---|---|
| `neutral` vs `camera` | 3 – 5 – 2 |
| `auto` vs `camera` | 5 – 1 – 4 |
| `auto` vs `system` | 3 – 3 – 4 |
| `auto` vs `neutral` | 6 – 1 – 3 |
| `ai` vs `auto` | 7 – 0 – 3 |
| `ai` vs `camera` | 8 – 0 – 2 |
| `ai` vs `system` | 8 – 1 – 1 |

The assistant's editing loop improved on `auto`, but the default recipe
still loses to the camera JPEG (3 – 5 – 2). High ISO is a particular weakness:
default noise reduction remains a fixed low value. `auto` only draws with
the macOS converter (3 – 3 – 4). Editors repeatedly noted that the camera's
rendering has stronger colour and contrast. These are small samples;
confidence intervals are wide, and a sealed set from the same camera and
selection is not evidence of general performance across cameras or scenes.

### Validation: what was found and fixed (24 photos)

The 24 photos had never been used for tuning before their first evaluation.
They were chosen by measurement in four classes of six: high ISO
(3200–6400), backlit, saturated sky, and snow and rime. Photos containing people were excluded
by on-device detection. They were first judged with the engine as released
in 0.1.1 plus the corrected camera rendering (`validation_before_fixes`).
That evaluation exposed weaknesses in highlight rendering, preview/export
consistency, noise reduction, demosaic false colour and `auto`.

Those areas were then fixed with these photos in view. Consequently,
`validation_after_fixes` is a development result and optimistic, not a
held-out estimate. The table records what the evaluation found and how
results changed after the fixes.

| Comparison | Before fixes (24 photos) | After fixes (24 photos, development) |
|---|---|---|
| `neutral` vs `camera` | 8 – 7 – 9 | 12 – 6 – 6 |
| `auto` vs `camera` | 9 – 11 – 4 | 20 – 2 – 2 |
| `auto` vs `system` | 10 – 12 – 2 | 17 – 4 – 3 |
| `auto` vs `neutral` | 12 – 7 – 5 | 14 – 1 – 9 |
| `ai` vs `auto` | 23 – 1 – 0 | 15 – 2 – 7 |
| `ai` vs `camera` | 17 – 5 – 2 | 23 – 0 – 1 |
| `ai` vs `system` | 15 – 6 – 3 | 22 – 0 – 2 |

Judge checks (accuracy and order consistency are the reported values,
shown as percentages without further rounding):

| Check | Sealed after fixes | Validation before fixes | Validation after fixes |
|---|---|---|---|
| Spoiled controls rejected | 100% over 32 votes | 90.6% over 64 votes | 98.4% over 64 votes |
| Identical-pair verdicts called a tie | 6 of 6 | 12 of 12 | 12 of 12 |
| Same verdict after swapping A and B | 80.2% | 83% | 86.5% |
| Position votes (A / B / tie) | 63 / 93 / 22 | 188 / 202 / 22 | 174 / 188 / 50 |
| Complete pairs judged (including checks) | 89 | 206 | 206 |
| Incomplete pairs | 0 | 0 | 0 |

Mean seconds on the Intel i7-10700 measurement machine (8 cores, macOS),
including rendering, encoding and writing. The preview long edge is 1400 px;
full-resolution exports use the default recipe. These are recorded workloads,
not timings for an assistant's entire editing session.

| Workload | Sealed after fixes | Validation before fixes | Validation after fixes |
|---|---|---|---|
| Default preview (`neutral`) | 4.619 | 0.406 | 4.579 |
| `auto` with preview | 7.342 | 1.044 | 7.101 |
| macOS converter (`system`) | 1.361 | 1.356 | 1.336 |
| Full-resolution JPEG export | 5.158 | 4.471 | 5.083 |

The 0.406 s pre-fix preview timing describes the removed path that skipped
noise reduction. It is not the speed of a current preview.

### Historical development results: 15 photos (0.1.0)

15 photos (the private set described under Limits), 258 judgements per
judge, every pair in both orders. Two judges from different vendors saw
the same blind tasks independently. Cells are wins – losses – ties for
the first-named variant, one outcome per photo.

Correction: All 15 private-set camera JPEGs were Adobe RGB but were shown
unconverted as sRGB in the published `camera` comparisons. This made the
camera rendering look duller and less saturated than it is. The
`neutral`/`auto`/`ai` vs `camera` rows therefore overstate rawsmith's advantage
and are withdrawn pending a rerun. Historical counts are retained.
Comparisons against `system`, `auto` vs `neutral` and `ai` vs `auto` are
unaffected, as are the three sRGB CC0 samples.

| Comparison | Claude | GPT-6 Luna |
|---|---|---|
| `neutral` vs `camera` **(withdrawn)** | 10 – 0 – 5 | 5 – 0 – 10 |
| `auto` vs `camera` **(withdrawn)** | 15 – 0 – 0 | 12 – 0 – 3 |
| `auto` vs `system` | 15 – 0 – 0 | 12 – 0 – 3 |
| `auto` vs `neutral` | 13 – 0 – 2 | 9 – 2 – 4 |
| `ai` vs `auto` | 12 – 1 – 2 | 6 – 0 – 9 |
| `ai` vs `camera` **(withdrawn)** | 15 – 0 – 0 | 12 – 0 – 3 |
| `ai` vs `system` | 15 – 0 – 0 | 13 – 0 – 2 |

A win is a photo whose two verdicts net out in the candidate's favour;
opposite verdicts in the two orders cancel to a tie. 95% Wilson intervals
on the win rate among decided photos are wide at this sample size: for
15 – 0 the lower bound is 80%, for 6 – 0 it is 61%, for 9 – 2 the
interval is 52–95%.

Judge checks for this run:

| Check | Claude | GPT-6 Luna |
|---|---|---|
| Spoiled controls rejected | 40 of 40 | 36 of 40 |
| Identical pairs called a tie | 8 of 8 | 8 of 8 |
| Same verdict after swapping A and B | 92% of pairs | 75% of pairs |
| Position votes (A / B / tie) | 122 / 118 / 18 | 88 / 140 / 30 |

In the unaffected comparisons, the two judges agree on direction and neither
prefers the system converter for any photo. The second judge
is the noisier instrument: it leans towards whichever image is shown
second, and because both orders are judged that lean turns into ties
rather than wins, which is why its counts are lower. Claude ran through
`claude -p`, GPT-6 Luna through `codex exec` with the two images attached.

Historical speed on the same machine, mean of the 15 files (33 MP each):
a 1400 px preview took 0.39 s; a full-resolution JPEG export took 4.3 s,
including encoding and writing. `rawsmith auto` with a preview took 1.05 s
and macOS `sips` 1.30 s for a picture of the same size. The preview and
`auto` figures used the removed approximate path that skipped noise reduction;
they do not describe current performance.

The `ai` recipes came from five independent assistant sessions (three
photos each) that were given only the skill file and the CLI, started
from `auto`, and used two to five rounds per photo.

### Limits

- All real-photo results here are from one camera. The sealed sample is
  small, the validation after fixes is optimistic, and the earlier private
  set is historical development evidence. The earlier set has 15 photos
  from one photographer:
  dusk and sunsets, hazy lake views, snow and rime in fog and sun. The
  intervals are wide, and the default look and `auto` were developed while
  looking at these same photos, so this is a development-set result, not a
  held-out one.
- An LLM judge measures plausibility of taste, not truth. It is good at
  rejecting obvious faults (see the control accuracy) and reasonably
  consistent, but two models are two opinions, not a panel of
  photographers. In the sealed and validation evaluations, editor and
  judge use the same Codex model at different effort levels. In the
  historical 15-photo run, the `ai` recipes were made by Claude, so the
  Claude judge shares a maker with them; the second judge does not.
- `camera` is a resized JPEG, so it has been compressed twice.

### Run it on your own photos

The default judge is now `gpt-6.1-sol` at `low` reasoning effort. In a
held-out reliability run, `gpt-6.1-sol` rejected 94% of spoiled controls and
kept 84% of verdicts after swapping image order, compared with 81% and 66%
for `gpt-6-luna`. The owner selected the more reliable judge for future
runs; the earlier 15-photo development table remains historical evidence.

```sh
cargo build --release
bench/photos.py render --raws ~/Pictures/raw --work ~/rawsmith-bench
bench/photos.py pairs  --work ~/rawsmith-bench
bench/photos.py judge  --work ~/rawsmith-bench     # Codex, gpt-6.1-sol, low effort, label codex
bench/photos.py report --work ~/rawsmith-bench
```

The default executable adapter, `bench/judge-codex.py`, requires Python 3.8+
and the `codex` CLI on PATH. It reads the prompt on stdin, attaches both images
with `codex exec -i`, and prints the reply. It runs read-only and ephemeral from
an empty temporary directory, ignores user configuration, and disables hooks,
apps, plugins and memories. Missing Codex, a failed run, or an empty reply
produces a stderr error and non-zero exit. The model is pinned by `--model`
(default `gpt-6.1-sol`), and reasoning effort by `--effort` (default `low`).
The adapter passes effort as `codex exec -c 'model_reasoning_effort="low"'`,
using the key in the [official Codex configuration reference](https://developers.openai.com/codex/config-reference)
and the `-c` override syntax from `codex exec --help`. Supported effort levels
depend on the selected model and Codex version. User configuration and
environment overrides do not select the adapter's model or effort.

```sh
# Choose a different model or effort under a new verdict label:
bench/photos.py judge --work ~/rawsmith-bench --model gpt-6-luna --effort high --name luna-high
```

`--judge-cmd` still accepts any shell command that reads the prompt on standard
input, can open the two image paths named in it, and prints a JSON verdict.
Custom commands must explicitly pin their own model and effort and declare
the same values with the harness's `--model` and `--effort` (which defaults to
`low`); the harness cannot infer either from an opaque shell command or
provider settings. These flags record configuration but do not modify custom
commands. Changing either declared value invalidates the file configuration.
Custom commands remain useful for other research judges, including Claude,
but project acceptance uses Codex exclusively:

```sh
bench/photos.py judge --work ~/rawsmith-bench --name custom-codex \
  --model gpt-6.1-sol --effort low \
  --judge-cmd '/path/to/rawsmith/bench/judge-codex.py --model gpt-6.1-sol --effort low'
```

The work directory must be outside the repository. Only judge synthetic or
owner-authorized photos; the adapter sends attached images to the provider.

Choose a new empty directory dedicated to this benchmark. The first command
creates `.rawsmith-benchmark.json`, which identifies the directory's format
and version. Later commands require that valid marker and reject symbolic
links within the directory. Re-running `render` replaces its generated
images and clears judging tasks; `pairs` replaces old tasks and blind images.
Both retain verdict files: resume and report validate them against the current
tasks and bytes. Keep your source RAWs and recipes elsewhere.

Work directories from older versions have no marker and are rejected without
changing their files. Keep the old directory for reference, choose a new
empty `--work` directory, then run `render`, `pairs` and `judge` there. Pass
your existing AI recipes with `render --recipes DIR` if needed. Do not add a
marker to an existing directory to bypass this check: the benchmark deletes
and replaces its own files on subsequent runs.
