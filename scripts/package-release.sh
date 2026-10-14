#!/usr/bin/env bash
set -euo pipefail

die() { printf 'rawsmith package: %s\n' "$*" >&2; exit 1; }

[[ $# -eq 3 ]] || die 'usage: package-release.sh TARGET vMAJOR.MINOR.PATCH OUTPUT_DIRECTORY'
target=$1
tag=$2
output=$3
case "$target" in
    x86_64-unknown-linux-musl) platform=linux-x86_64 ;;
    aarch64-unknown-linux-musl) platform=linux-arm64 ;;
    x86_64-apple-darwin) platform=macos-x86_64 ;;
    aarch64-apple-darwin) platform=macos-arm64 ;;
    *) die "unsupported target: $target" ;;
esac
[[ "$tag" =~ ^v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$ ]] || die 'tag must be vMAJOR.MINOR.PATCH'
version=${tag#v}
root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)

# Read just the package table; no build, network access or photograph is needed.
package_version=$(python3 - "$root/Cargo.toml" <<'PY'
import pathlib, re, sys
text = pathlib.Path(sys.argv[1]).read_text()
table = re.search(r'(?ms)^\[package\]\s*\n(.*?)(?=^\[|\Z)', text)
version = re.search(r'(?m)^version\s*=\s*"([^"]+)"\s*$', table[1] if table else '')
if version is None:
    sys.exit('Cannot read the Cargo package version')
print(version[1])
PY
)
[[ "$package_version" == "$version" ]] || die "tag $tag does not match Cargo version $package_version"
binary="$root/target/$target/release/rawsmith"
[[ -x "$binary" ]] || die "missing executable: $binary"
[[ $("$binary" --version) == "rawsmith $version" ]] || die 'binary version does not match the release'
"$binary" schema | python3 -c 'import json, sys; schema = json.load(sys.stdin); assert isinstance(schema, dict) and schema, "empty or invalid schema"'

mkdir -p "$output"
output=$(cd "$output" && pwd)
archive="$output/rawsmith-$platform.tar.gz"
[[ ! -e "$archive" ]] || die "archive already exists: $archive"
stage=$(mktemp -d "$output/.rawsmith-package.XXXXXX")
trap 'rm -rf "$stage"' EXIT
mkdir -p "$stage/bin" "$stage/skill/rawsmith"
cp "$binary" "$stage/bin/rawsmith"
cp "$root/skill/rawsmith/scripts/select_candidate.py" "$stage/bin/rawsmith-select"
cp "$root/skill/rawsmith/SKILL.md" "$stage/skill/rawsmith/SKILL.md"
cp "$root/LICENSE-MIT" "$root/LICENSE-APACHE" "$root/README.md" "$stage/"
printf '%s\n' "$version" > "$stage/VERSION"
chmod 755 "$stage/bin/rawsmith"
chmod 755 "$stage/bin/rawsmith-select"
chmod 644 "$stage/skill/rawsmith/SKILL.md" "$stage/LICENSE-MIT" "$stage/LICENSE-APACHE" "$stage/README.md" "$stage/VERSION"
# macOS otherwise adds AppleDouble metadata files (._*) to the distribution.
COPYFILE_DISABLE=1 tar -czf "$stage/archive.tar.gz" -C "$stage" bin skill LICENSE-MIT LICENSE-APACHE README.md VERSION
mv "$stage/archive.tar.gz" "$archive"
printf '%s\n' "$archive"
