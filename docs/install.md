# Install rawsmith

Install the latest prebuilt release and the skill for both Claude Code and
Codex:

```sh
curl -fsSL https://github.com/Lewin671/rawsmith/releases/latest/download/install.sh | sh
```

Supports macOS 11+ (Apple Silicon and Intel) and Linux (ARM64 and x86_64;
static musl binaries). Requires `curl`, `tar`, `awk`, `mktemp` and
`sha256sum` or `shasum`.
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
# Other options: --skills claude | --skills none | --version 0.1.1 | --bin-dir "$HOME/bin"
```

Re-running updates installer-managed skills. Locally customized skills
are preserved; `--force` saves a backup before replacing them. See
[release downloads](https://github.com/Lewin671/rawsmith/releases) or
`install.sh --help` for all options.

For a manual download, choose `rawsmith-<platform>-<architecture>.tar.gz`:
`linux-x86_64`, `linux-arm64`, `macos-x86_64` (Intel), or `macos-arm64`
(Apple Silicon). Linux binaries use static musl. The installer also
supports `--version 0.1.0`, whose original downloads used Rust target names.

To build from source instead:

```sh
git clone https://github.com/Lewin671/rawsmith
cd rawsmith
cargo install --locked --path .
```

Source builds require a recent stable Rust toolchain. No other dependencies.

## Assistant skills

[`skill/rawsmith/SKILL.md`](../skill/rawsmith/SKILL.md) teaches an assistant
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

