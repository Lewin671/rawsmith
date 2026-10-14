# Contributing

Thanks for your interest in rawsmith. Bug reports, camera support and
quality improvements are all welcome.

## Building and testing

```sh
cargo build --release
cargo test --release
cargo run --release --example quality -- --check
cargo fmt --check && cargo clippy --all-targets -- -D warnings
python3 -m unittest discover -s tests -p 'test_*.py'
```

Tests build their own synthetic RAW files (`src/synthetic.rs`), so no
photographs are needed. Python tests check the benchmark's filesystem
safety without rendering photos or invoking a judge. CI runs these checks.

## Changing rendering performance

Performance measurements run only on the designated reference host, one at
a time, with builds/tests excluded from the measurement period. Follow the
workspace shared-host scheduling rules. Never substitute a local timing.

For stage diagnostics build with `--features render-profile` and set
`RAWSMITH_PROFILE=1`; diagnostics go to stderr without changing report JSON.
See [performance measurements](docs/performance.md) for fields, workload
runner and allocation/RSS distinctions. Pure optimisations must preserve
pixel bits, reports and encoded bytes. The render hashes in
`tests/determinism.rs` are the Intel macOS baseline; record them again only
for a change that is meant to alter pixels, and say so in the test; portable original-filter
and scheduling controls also run on other platforms.

## Changing image quality

Anything that changes pixels should come with evidence:

- `examples/quality.rs` measures the pipeline against known answers
  (demosaic PSNR, highlight reconstruction error, colour accuracy, noise
  reduction, sharpening overshoot). `--check` fails when a result falls
  below its floor. If your change is meant to improve a number, say which
  one and by how much; if it needs a new scene, add it.
- `bench/photos.py` compares renderings of real photos with an LLM judge.
  Run it on your own RAW files when you change the look (tone curve,
  defaults, `auto`). See [docs/benchmark.md](docs/benchmark.md).

Prefer one focused improvement with a measured effect over a broad rewrite.

Changes to the editing skill, rendering defaults or `auto` must not replace
the accepted baseline when a photo comparison shows a confirmed regression.
Compare old and new edits with the same engine where applicable, editing
model, task and rendering size/path; keep the baseline judge criteria fixed.
Check each photo, not just aggregate wins: gains on one scene do not excuse
a loss on another. Recheck inconsistent verdicts and unresolved defects
before acceptance. Until the candidate demonstrates improvement without
confirmed losses on the checked scenes, retain the baseline as the default.
Code review and passing synthetic tests alone do not establish better photos;
report the sample coverage and remaining uncertainty.

Project acceptance uses Codex for reviews, real-photo judging and the
complete editing workflow. `bench/photos.py judge` defaults to the repository
Codex adapter with `gpt-6.1-sol` at `low` reasoning effort; verify the adapter
and its explicit `--model` and `--effort` configuration before using a run for
acceptance. Synthetic tests exercise
cache validation and adapter failures without calling a real judge.
Published Claude results describe earlier development runs. Claude Code
skill installation compatibility can be inspected by Codex.

## Adding a camera

1. Add its colour matrix (XYZ D65 to camera RGB, the convention used by
   DNG `ColorMatrix2`) to `CAMERAS` in `src/color.rs`, with its source.
2. Check that a RAW file from the camera decodes: sensor samples should
   match another decoder exactly (`examples/dump.rs` writes them out).
3. Render a file and compare it with the camera's embedded JPEG
   (`rawsmith extract-jpeg`) for geometry, colour and brightness.

## Photograph permissions

Do not add RAW files or pictures rendered from someone's photos to the
repository, in tests, issues or documentation, unless they are published
under a licence that allows it (for example CC0), or the owner explicitly
authorizes their inclusion in this repository. Document the source and
permission in `docs/images/README.md`; documentation photo permissions
are separate from the code licence. `.gitignore` blocks common image
extensions as a safety net; `bench/photos.py` refuses to write inside the
repository.

## Style

- Code and comments in English; `cargo fmt` decides formatting.
- Comments explain why a step exists or what a constant means, not what
  the next line does.
- Every recipe parameter needs a range and a description in
  `params::fields()`; that table is what `rawsmith schema` shows to
  assistants.

## Licence

By contributing you agree that your work is licensed under the project's
dual MIT / Apache-2.0 terms, except photographs with separately documented
permissions.
