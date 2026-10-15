# Releasing rawsmith

Stable releases are built from an existing `vMAJOR.MINOR.PATCH` tag. The
tag must match the version in Cargo.toml exactly. All release jobs check
out the resolved commit, not a moving branch.

1. Update Cargo.toml and Cargo.lock for a new version and commit the
   changes with the release notes in CHANGELOG.md. Run the checks in
   CONTRIBUTING.md.
2. Push main, create an annotated version tag and push that tag:

   ```sh
   git push origin main
   git tag -a v0.1.1 -m 'rawsmith v0.1.1'
   git push origin v0.1.1
   ```

   Use the new version for later releases. Never move a published tag.

3. Watch the Release workflow. It validates the version, runs formatting,
   Clippy, Rust and Python tests and the synthetic quality gate, then
   builds and tests each target on a native GitHub runner:

   | Platform | Download | Rust target |
   |---|---|---|
   | Linux x86_64 | `rawsmith-linux-x86_64.tar.gz` | `x86_64-unknown-linux-musl` |
   | Linux ARM64 | `rawsmith-linux-arm64.tar.gz` | `aarch64-unknown-linux-musl` |
   | macOS Intel | `rawsmith-macos-x86_64.tar.gz` | `x86_64-apple-darwin` |
   | macOS Apple Silicon | `rawsmith-macos-arm64.tar.gz` | `aarch64-apple-darwin` |

   Linux binaries use static musl; macOS binaries target macOS 11 or
   later. Hosted jobs run correctness checks, not performance measurements.

4. Only after every target passes does the publishing job create a draft,
   attach four archives, install.sh and SHA256SUMS, then publish it as
   latest. Only that job receives repository write permission.

Each archive contains `bin/rawsmith`, `skill/rawsmith/SKILL.md`, VERSION,
the two code licences and README.md. Photos, private renders, image
metadata and repository development files are not packaged.

Public download names describe the platform and CPU architecture; Rust
target names remain internal build settings. The installer uses the old
target-based names only for the already-published v0.1.0 release.

After publication, test the README's curl command in a disposable install
directory and run the installed binary's `--version` and `schema`. Check
the Claude Code and Codex skill files against the release source. Keep
this smoke test isolated from existing user installations.

## Retry a failed release

Re-run the failed workflow, or dispatch Release with the existing stable
tag. For a workflow-only fix, push the corrected workflow to main and
dispatch it from main with the original tag; this changes the build
configuration while preserving the tagged product source and version.
A draft is resumable only when its tag and embedded release-commit marker
match the build and it has no unexpected attachments. Expected draft assets are replaced as a set
before publication. A published release is never overwritten; publish a
new version for fixes.

The workflow verifies the existing tag and records its commit in the
release body. It omits the API's optional target_commitish field, which
is unused for existing tags but can trigger an additional workflow-write
permission check for older commits. The built source remains pinned to
the tag's resolved commit; no extra token or repository permission is
needed. See [GitHub's release API](https://docs.github.com/en/rest/releases/releases#create-a-release).

If the failure needs a source fix, discard the unpublished draft and
delete the failed tag only after verifying that it was never published
or used for distribution. Prefer a fresh version tag when in doubt.
