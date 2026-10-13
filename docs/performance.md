# Render performance

Two rounds of work are recorded here. The second, for 0.4.0, comes first;
the pixel-preserving optimisation it builds on follows unchanged.

## 0.4.0: reusing the developed frame

An assistant renders one photo many times while only finishing settings
change. The frame after noise reduction depends on the RAW, white balance,
lens corrections, defringe and denoise alone, so the CLI now keeps it on
disk between commands ([how](cli.md#reusing-the-developed-frame)) and later
renders start from it. The same release changed luminance noise reduction
on purpose, which costs time when a frame is developed.

Same host, method and two files as below; medians of three serial runs of
`scripts/perf-render.py` with `--features render-profile`. "First" develops
the photo with reuse off; "later" is a render whose frame was stored by an
earlier command. 0.3.0 has no reuse, so every render was a first render.

| Workload | ISO 100 s, 0.3.0 → first → later | ISO 100 RSS GB | ISO 4000 s, 0.3.0 → first → later | ISO 4000 RSS GB |
|---|---:|---:|---:|---:|
| Native JPEG | 3.49 → 3.50 → 1.86 | 2.09 → 2.09 → 1.18 | 4.62 → 4.88 → 1.90 | 2.30 → 2.30 → 1.19 |
| 1400 px preview | 2.89 → 2.90 → 1.29 | 1.91 → 1.91 → 0.97 | 3.97 → 4.25 → 1.29 | 2.10 → 2.10 → 0.97 |
| auto + 1400 px preview | 5.80 → 4.65 | 2.18 → 2.15 | 7.18 → 6.22 | 2.35 → 2.30 |

A complete loop on an uninstrumented build (auto with preview, compare,
three edits with previews including a crop, colour and a mask, compare, two
native regions, native export; two repetitions agreed within 0.2 s) took
38.3 s on 0.3.0 and 25.4 s with reuse at ISO 100, and 47.3 s and 27.6 s at
ISO 4000. An edit with a preview fell from 3.65 and 4.63 s to 2.07 s. All
nine outputs at ISO 100 are byte-identical to 0.3.0's; at ISO 4000 they
differ because noise reduction does.

What changed:

- A later render reads the stored frame (0.13 s for 393 MB) instead of
  developing it (1.7 s at ISO 100, 3.0 s at ISO 4000), and never holds the
  mosaic or the denoiser's planes, which is where the RSS goes.
- Luminance noise reduction is wavelet shrinkage (see
  [pipeline.md](pipeline.md)). At ISO 4000 the denoise stage takes 2.36 s
  where it took 2.04 s; at ISO 100, where it does not run, the stage is
  unchanged.
- `auto` measured one candidate rendering on a single thread, with a
  colour conversion and two large sorts; conversions and sorts are now
  parallel with sums kept in pixel order, and repeated candidates are not
  rendered twice. Recipes and decisions are identical on all 34 development
  photos. The camera reference is decoded at the smallest size the analysis
  needs instead of in full (0.08–0.12 s instead of 0.44–0.50 s), which
  moves its measured tones by a few tenths of a lightness point and changed
  one setting by one step in 2 of 34 recipes. Analysis fell from 1.9–2.2 s
  to 0.9–1.1 s.

Correctness: the commit that added reuse, before noise reduction changed,
passes the historical render hashes on the reference host, and CLI tests
compare every output and report with and without reuse across sizes,
regions, crops, rotation, masks, curves and both formats. The hashes in
`tests/determinism.rs` were recorded again for the new noise reduction.

Tried and not kept:

- Replacing the system library's cube root and power in finishing with
  plain-arithmetic roots accurate to float rounding. Finishing a later
  render is 0.9–1.5 s, about 40% of its CPU time in `powf` and `cbrtf`. On the
  reference host the replacement made the colour stage slower (0.88 →
  1.30 s) and was removed.
- Developing previews at reduced resolution remains deferred for the
  reasons under "Correctness and remaining targets"; with reuse the
  optimistic saving is the finishing time of a later render, about 1 s.

The 2 GB RSS objective is met by later renders and remains unmet by first
renders at ISO 4000 (2.30 GB).

# Pixel-preserving render optimisation (0.3.0)

Measured on the reference Intel Core i7-10700 (8 cores / 16 threads),
macOS 12.6, Rust 1.99.0, with two 33 MP Sony A7 IV files at ISO 100 and ISO 4000. The baseline
is main `631d308` with diagnostics added, before changing rendering code.
Each cell is the median of three serial `/usr/bin/time -l` runs. Builds,
tests and other measurements were excluded from measurement periods.
Outputs and per-run logs remain in `/tmp` on the reference host; no private
photos or identifying source paths are included here.

Both builds used `--release --features render-profile` and
`RAWSMITH_PROFILE=1`. Allocation tracking adds overhead, so these are
instrumented workload comparisons, not a promise for other machines.
GB and MB below are decimal (10^9 and 10^6 bytes).

## Complete CLI workloads

| Workload | ISO 100 seconds, before → after | ISO 100 RSS GB, before → after | ISO 4000 seconds, before → after | ISO 4000 RSS GB, before → after |
|---|---:|---:|---:|---:|
| Native JPEG | 5.00 → 3.49 | 3.01 → 2.09 | 7.08 → 4.62 | 3.41 → 2.30 |
| 1400 px preview | 4.54 → 2.89 | 2.83 → 1.91 | 6.39 → 3.97 | 3.22 → 2.10 |
| auto + 1400 px preview | 9.37 → 5.80 | 3.56 → 2.18 | 11.82 → 7.18 | 3.88 → 2.35 |

## Stages of a native JPEG export

Times include the entire named stage. Allocations are the sum of requested
Rust heap bytes during that stage, including scratch buffers subsequently
freed. They are not simultaneous live memory or RSS. Independent medians
need not sum to the median complete CLI time; output statistics and file
I/O are outside the listed stages.

| Stage | ISO 100 ms, before → after | ISO 100 allocated MB, before → after | ISO 4000 ms, before → after | ISO 4000 allocated MB, before → after |
|---|---:|---:|---:|---:|
| decode | 21.9 → 21.2 | 66.0 → 66.0 | 22.2 → 21.0 | 66.0 → 66.0 |
| noise/prepare | 30.1 → 31.1 | 131.1 → 131.1 | 30.3 → 31.0 | 131.1 → 131.1 |
| demosaic | 453.7 → 92.1 | 523.9 → 523.9 | 548.8 → 101.0 | 523.9 → 523.9 |
| sensor/highlights | 43.5 → 43.9 | 32.7 → 32.7 | 46.0 → 43.3 | 32.7 → 32.7 |
| lens/matrix/fringe | 457.8 → 500.4 | 392.9 → 392.9 | 536.8 → 493.9 | 392.9 → 392.9 |
| denoise | 2046.4 → 1052.6 | 8643.7 → 2226.4 | 3801.3 → 2041.4 | 15814.8 → 4183.4 |
| geometry/scene resize | 0.0 → 0.0 | 0.0 → 0.0 | 0.0 → 0.0 | 0.0 → 0.0 |
| exposure/local tone | 85.0 → 30.9 | 392.9 → 0.0 | 87.9 → 30.8 | 392.9 → 0.0 |
| filmic/colour/gamut | 443.3 → 457.9 | 0.0 → 0.0 | 444.1 → 462.0 | 0.0 → 0.0 |
| sharpen/clamp | 527.5 → 383.0 | 1702.6 → 654.8 | 533.9 → 387.4 | 1702.6 → 654.8 |
| output resize | 0.0 → 0.0 | 0.0 → 0.0 | 0.0 → 0.0 | 0.0 → 0.0 |
| encode | 671.2 → 683.9 | 418.3 → 418.3 | 794.3 → 792.9 | 443.7 → 443.7 |

## What changed

- Demosaic mirror indexing returns in-range coordinates directly, avoiding
  integer division at every interior neighbourhood sample.
- Scalar planes resize as scalar data, keeping the original area weights
  and horizontal/vertical accumulation order instead of expanding to RGB.
- Box filters process 32 neighbouring columns together and reuse consumed
  input storage. Replicated edges, signed-zero initialization and each
  column's running-sum order are unchanged. Gaussian passes consume their
  disposable intermediates.
- Chroma denoising computes guide statistics once for both channels,
  reuses coefficient planes, consumes original chroma after preserving the
  small coarse guides, and releases unused planes before reconstruction.
  Chroma-only luminance restoration stays per-pixel with the same
  square-root/square round trip. High-strength luminance filtering releases
  guide statistics before coefficient smoothing.
- Output rendering finishes the owned scene instead of cloning it.
  Automatic candidates still borrow and copy their shared scene.
- Gamut mapping retains the last accepted bisection result instead of
  converting that same colour again.
- Camera JPEG pixel conversion uses parallel independent pixel work;
  decoding, framing, colour arithmetic and resize order stay unchanged.
  `auto` releases original file bytes before native preview development.

Full-frame filter support remains intact for crops and regions. There is
no reduced-resolution output development, changed denoise policy, altered
transfer function, different JPEG quality or changed parameter/report API.

## Correctness and remaining targets

The 24 historical FNV-1a hashes in `tests/determinism.rs` were recorded from
main on the reference host before optimisation. They include dimensions,
all float pixel bits, JPEG and PNG bytes across default, automatic, edited
and high-denoise/rotated recipes, native/400/1 px sizes and cropped regions.
The historical golden runs on Intel macOS only: transcendental float
functions can differ across platforms, consistent with the documented
same-binary/same-platform rendering contract.

Portable tests retain the Rayon scheduling controls and compare rewritten
box/guided filters and scalar resizing against original arithmetic,
including tiny dimensions, partial tiles, signed zero and windows larger
than the image. All 18 measured real-photo JPEGs match main byte for byte.
Their JSON reports also match, excluding elapsed time and output/recipe
paths. Existing CLI tests cover saved/trial edits, auto, comparison,
regions, metadata and failure preservation.

Validation passed on local ARM macOS and the Intel reference host:
`cargo test --release`, `quality --check`, fmt, clippy with all targets and
features, and 73 Python tests. The reference-host golden also passes in
the ordinary build. No photo judging or release acceptance was performed.

The ISO 100 export and auto time targets are reached, and previews are
faster than exports for both files. ISO 4000 export/auto and the strict
2 GB RSS target remain unmet. Requested live heap peaks are lower than RSS;
allocator retention, stacks and other process memory still count in RSS.

Potential pixel-changing options are deferred:

- Earlier preview resizing would save native processing, but changes noise,
  spatial support, nonlinear tone/gamut results and dithering. From the
  measured native denoise, lens, colour and sharpening stages and the
  approximately 25:1 pixel reduction at 1400 px, the optimistic saving is
  about 2.3 s at ISO 100 / 3.2 s at ISO 4000. This is an upper-bound estimate,
  not a measured alternative implementation; filter overhead and quality
  constraints reduce the achievable saving. It was not implemented.
- JPEG encoder AVX2 was tried and rejected by the historical byte hashes.
  Its saving was not benchmarked; the measured scalar encoding stages
  (about 0.68 / 0.79 s) bound the possible whole-export saving. The SIMD
  feature is disabled.

## Reproducing and interpreting diagnostics

Use the workspace's designated reference-host runner, which waits for an
active shared-host measurement. Do not take or modify queue locks. Never
run this measurement script locally, and run one workload at a time.
Inside the synced reference checkout:

```sh
cargo build --release --features render-profile
python3 scripts/perf-render.py after /path/to/reference/raw-directory
```

The script uses the two designated reference filenames, isolated `{}`
recipes, the default JPEG quality (92), native JPEG exports, 1400 px previews, and `auto` with
1400 px output, all with frame reuse off, then `reuse-export` and
`reuse-preview` against a frame it stores first in its own directory. It writes images, recipes, stdout, stderr and summary JSON
under `/tmp/rawsmith-perf-<label>`. A label contains only letters, digits,
underscores or hyphens. It owns no host scheduler or lock.

For one command, set `RAWSMITH_PROFILE=1`. Stderr has six tab-separated
fields: `profile`, stage name, wall milliseconds, newly requested heap
bytes, live requested heap bytes at stage exit, and cumulative process
peak requested heap bytes. The last column is a process high-water mark,
not the isolated stage's peak. Nested `auto total` includes its child
stages; do not add it to them. Concurrent library calls share counters.
Without the feature, wall diagnostics work but allocation fields are zero.
Without the environment variable, normal JSON and stderr are unchanged;
ordinary builds contain no tracking allocator.
