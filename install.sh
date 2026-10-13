#!/bin/sh
# Install a verified release and its agent skill without Rust or sudo.
set -eu

usage() {
    cat <<'EOF'
Usage: install.sh [options]

  --version VERSION          Install a stable version (default: latest release)
  --bin-dir DIR              Binary directory (default: ~/.local/bin)
  --skills both|claude|codex|none
                             Agent skills to install (default: both)
  --claude-skills-dir DIR     Override Claude Code's skills directory
  --codex-skills-dir DIR      Override Codex's skills directory
  --force                    Back up and replace a customized rawsmith skill
  --help                     Show this help

Default skill directories:
  Claude Code: ${CLAUDE_CONFIG_DIR:-~/.claude}/skills/rawsmith
  Codex:       ~/.agents/skills/rawsmith

The installer does not edit shell profiles. Add the binary directory to PATH
if necessary. Only macOS and Linux, on x86_64 or ARM64, are supported.
EOF
}

fail() { printf 'rawsmith: %s\n' "$*" >&2; exit 1; }

cleanup() {
    for directory in "$download_dir" "$bin_stage_dir" "$claude_stage_dir" "$codex_stage_dir"; do
        if [ -n "$directory" ]; then rm -rf -- "$directory"; fi
    done
}

sha256() {
    if command -v sha256sum >/dev/null 2>&1; then
        checksum=$(sha256sum "$1") || fail "cannot read $1 for checksum verification"
    else
        checksum=$(shasum -a 256 "$1") || fail "cannot read $1 for checksum verification"
    fi
    printf '%s\n' "$checksum" | awk '{print $1}'
}

download() {
    curl --fail --silent --show-error --location --proto '=https' --tlsv1.2 \
        --retry 3 --connect-timeout 15 --max-time 180 --output "$2" "$1" \
        || fail "cannot download $1; check the version and your network connection"
}

regular_destination() {
    if [ -L "$1" ] || { [ -e "$1" ] && [ ! -f "$1" ]; }; then
        fail "$1 is not a regular file; choose another destination"
    fi
}

check_skill() {
    destination=$1
    if [ -L "$destination" ] || { [ -e "$destination" ] && [ ! -d "$destination" ]; }; then
        fail "$destination is not a regular directory; choose another skills directory"
    fi
    regular_destination "$destination/SKILL.md"
    regular_destination "$destination/.rawsmith-source.sha256"
    if [ -f "$destination/SKILL.md" ]; then
        current_hash=$(sha256 "$destination/SKILL.md")
        if [ "$current_hash" != "$skill_hash" ]; then
            managed_hash=''
            if [ -f "$destination/.rawsmith-source.sha256" ]; then
                managed_hash=$(cat "$destination/.rawsmith-source.sha256")
            fi
            if [ "$current_hash" != "$managed_hash" ] && [ "$force" != yes ]; then
                fail "$destination/SKILL.md has local changes; use --force to back it up and replace it"
            fi
        fi
    fi
}

stage_skill() {
    destination=$1
    mkdir -p "$destination"
    staging=$(mktemp -d "$destination/.rawsmith-install.XXXXXX")
    # Return the owned directory immediately; cleanup must know it even if a copy fails.
    printf '%s\n' "$staging"
}

prepare_skill() {
    cp "$download_dir/unpack/skill/rawsmith/SKILL.md" "$1/SKILL.md"
    chmod 644 "$1/SKILL.md"
    printf '%s\n' "$skill_hash" > "$1/.rawsmith-source.sha256"
}

commit_skill() {
    destination=$1
    staging=$2
    if [ "$force" = yes ] && [ -f "$destination/SKILL.md" ] && \
        [ "$(sha256 "$destination/SKILL.md")" != "$skill_hash" ]; then
        saved=$(mktemp "$destination/SKILL.md.backup.XXXXXX")
        cp "$destination/SKILL.md" "$saved"
        printf 'Saved previous skill: %s\n' "$saved"
    fi
    mv -f "$staging/SKILL.md" "$destination/SKILL.md"
    mv -f "$staging/.rawsmith-source.sha256" "$destination/.rawsmith-source.sha256"
    printf 'Installed skill: %s\n' "$destination/SKILL.md"
}

main() {
    download_dir='' bin_stage_dir='' claude_stage_dir='' codex_stage_dir=''
    trap cleanup EXIT
    trap 'exit 1' HUP INT TERM
    version='' skills=both force=no
    bin_dir=${HOME:?HOME must be set}/.local/bin
    claude_root=${CLAUDE_CONFIG_DIR:-$HOME/.claude}/skills
    codex_root=$HOME/.agents/skills
    while [ "$#" -gt 0 ]; do
        case "$1" in
            --version|--bin-dir|--skills|--claude-skills-dir|--codex-skills-dir)
                [ "$#" -ge 2 ] && [ -n "$2" ] || fail "$1 needs a value"
                case "$1" in
                    --version) version=$2 ;;
                    --bin-dir) bin_dir=$2 ;;
                    --skills) skills=$2 ;;
                    --claude-skills-dir) claude_root=$2 ;;
                    --codex-skills-dir) codex_root=$2 ;;
                esac
                shift 2 ;;
            --force) force=yes; shift ;;
            --help|-h) usage; return ;;
            *) fail "unknown option $1; use --help" ;;
        esac
    done
    case "$skills" in both|claude|codex|none) ;; *) fail "--skills must be both, claude, codex or none" ;; esac
    case "$bin_dir" in /*) ;; *) bin_dir=$PWD/$bin_dir ;; esac
    case "$claude_root" in /*) ;; *) claude_root=$PWD/$claude_root ;; esac
    case "$codex_root" in /*) ;; *) codex_root=$PWD/$codex_root ;; esac
    for tool in curl tar awk mktemp; do command -v "$tool" >/dev/null 2>&1 || fail "$tool is required"; done
    command -v sha256sum >/dev/null 2>&1 || command -v shasum >/dev/null 2>&1 || fail "sha256sum or shasum is required"
    case "$(uname -s):$(uname -m)" in
        Darwin:arm64|Darwin:aarch64) platform=macos-arm64; legacy_target=aarch64-apple-darwin ;;
        Darwin:x86_64|Darwin:amd64) platform=macos-x86_64; legacy_target=x86_64-apple-darwin ;;
        Linux:arm64|Linux:aarch64) platform=linux-arm64; legacy_target=aarch64-unknown-linux-musl ;;
        Linux:x86_64|Linux:amd64) platform=linux-x86_64; legacy_target=x86_64-unknown-linux-musl ;;
        *) fail "unsupported system; use a macOS or Linux x86_64/ARM64 release, or build from source" ;;
    esac
    repository=https://github.com/Lewin671/rawsmith
    if [ -z "$version" ]; then
        latest=$(curl --fail --silent --show-error --location --proto '=https' --tlsv1.2 \
            --retry 3 --connect-timeout 15 --max-time 180 --output /dev/null \
            --write-out '%{url_effective}' "$repository/releases/latest") \
            || fail "cannot find a published release; check your network connection"
        case "$latest" in
            "$repository/releases/tag/"*) version=${latest##*/} ;;
            *) fail "unexpected latest release URL: $latest" ;;
        esac
    fi
    case "$version" in v*) tag=$version ;; *) tag=v$version ;; esac
    printf '%s\n' "$tag" | LC_ALL=C grep -Eq '^v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$' \
        || fail "--version must be a stable version such as 0.1.0 or v0.1.0"
    version=${tag#v}
    archive=rawsmith-$platform.tar.gz
    # The first published release used Rust target triples as asset names.
    if [ "$version" = 0.1.0 ]; then archive=rawsmith-$legacy_target.tar.gz; fi
    download_dir=$(mktemp -d "${TMPDIR:-/tmp}/rawsmith-install.XXXXXX")
    base=$repository/releases/download/$tag
    printf 'Downloading rawsmith %s for %s\n' "$version" "$platform"
    download "$base/$archive" "$download_dir/$archive"
    download "$base/SHA256SUMS" "$download_dir/SHA256SUMS"
    expected=$(awk -v archive="$archive" '$2 == archive {print $1}' "$download_dir/SHA256SUMS")
    [ "${#expected}" -eq 64 ] || fail "missing or ambiguous checksum for $archive"
    case "$expected" in *[!0-9a-fA-F]*) fail "invalid checksum for $archive" ;; esac
    [ "$(sha256 "$download_dir/$archive")" = "$expected" ] || fail "checksum mismatch; nothing was installed"

    # Accept only the exact release layout before extracting into our scratch directory.
    tar -tzf "$download_dir/$archive" > "$download_dir/members" || fail "cannot read release archive"
    while IFS= read -r member; do
        case "$member" in
            bin|bin/|bin/rawsmith|skill|skill/|skill/rawsmith|skill/rawsmith/|skill/rawsmith/SKILL.md|VERSION|LICENSE-MIT|LICENSE-APACHE|README.md) ;;
            *) fail "unexpected archive member: $member" ;;
        esac
    done < "$download_dir/members"
    tar -tvzf "$download_dir/$archive" > "$download_dir/types" || fail "cannot inspect release archive"
    while IFS= read -r entry; do
        case "$entry" in -*|d*) ;; *) fail "release archive contains a link or special file" ;; esac
    done < "$download_dir/types"
    mkdir "$download_dir/unpack"
    tar -xzf "$download_dir/$archive" -C "$download_dir/unpack" --no-same-owner --no-same-permissions \
        || fail "cannot unpack release"
    binary=$download_dir/unpack/bin/rawsmith
    skill=$download_dir/unpack/skill/rawsmith/SKILL.md
    [ -f "$binary" ] && [ -f "$skill" ] && [ -f "$download_dir/unpack/VERSION" ] || fail "release is missing required files"
    [ "$(cat "$download_dir/unpack/VERSION")" = "$version" ] || fail "release version does not match $tag"
    chmod 755 "$binary"
    [ "$("$binary" --version)" = "rawsmith $version" ] || fail "release binary cannot run or has the wrong version"
    skill_hash=$(sha256 "$skill")

    regular_destination "$bin_dir/rawsmith"
    claude_destination=$claude_root/rawsmith
    codex_destination=$codex_root/rawsmith
    case "$skills" in both|claude) check_skill "$claude_destination" ;; esac
    case "$skills" in both|codex) check_skill "$codex_destination" ;; esac
    # Stage every destination before replacing the existing binary or skills.
    mkdir -p "$bin_dir"
    bin_stage_dir=$(mktemp -d "$bin_dir/.rawsmith-install.XXXXXX")
    cp "$binary" "$bin_stage_dir/rawsmith"
    chmod 755 "$bin_stage_dir/rawsmith"
    case "$skills" in both|claude)
        claude_stage_dir=$(stage_skill "$claude_destination")
        prepare_skill "$claude_stage_dir" ;;
    esac
    case "$skills" in both|codex)
        codex_stage_dir=$(stage_skill "$codex_destination")
        prepare_skill "$codex_stage_dir" ;;
    esac
    if [ -n "$claude_stage_dir" ]; then commit_skill "$claude_destination" "$claude_stage_dir"; fi
    if [ -n "$codex_stage_dir" ]; then commit_skill "$codex_destination" "$codex_stage_dir"; fi
    mv -f "$bin_stage_dir/rawsmith" "$bin_dir/rawsmith"
    printf 'Installed rawsmith %s: %s/rawsmith\n' "$version" "$bin_dir"
    case ":${PATH:-}:" in
        *":$bin_dir:"*) ;;
        *) printf 'Add this directory to PATH: %s\n' "$bin_dir" ;;
    esac
    printf 'Ask your agent to edit a RAW photo with rawsmith. If the skill is missing, reload skills or restart the agent.\n'
}

# Defining main first prevents a truncated curl response from installing anything.
main "$@"
