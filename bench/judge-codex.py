#!/usr/bin/env python3
"""Codex photo judge: prompt on stdin, reply on stdout; default gpt-6-luna."""
import argparse
import os
import shutil
import subprocess
import sys
import tempfile


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--model", default="gpt-6-luna", help="explicit Codex model (default: gpt-6-luna; environment model overrides are ignored)")
    args = parser.parse_args()
    if not args.model.strip():
        parser.error("--model must not be empty")
    codex = shutil.which("codex")
    if not codex:
        parser.exit(1, "error: codex not found on PATH; install the Codex CLI before judging\n")
    prompt = sys.stdin.read()
    images = []
    for side in "AB":
        lines = [line[len(f"Image {side}: "):] for line in prompt.splitlines() if line.startswith(f"Image {side}: ")]
        if len(lines) != 1 or not lines[0]:
            parser.exit(1, f"error: prompt must name exactly one Image {side}\n")
        images.append(os.path.abspath(lines[0]))
    # An empty working root avoids loading project instructions. Auth stays in
    # the user's Codex home, but settings and optional integrations are disabled.
    with tempfile.TemporaryDirectory(prefix="rawsmith-judge-") as work:
        reply = os.path.join(work, "reply.txt")
        command = [codex, "exec", "-m", args.model, "-s", "read-only", "--ephemeral",
                   "--skip-git-repo-check", "--ignore-user-config",
                   "--disable", "hooks", "--disable", "apps", "--disable", "plugins", "--disable", "memories",
                   "-i", images[0], "-i", images[1], "-o", reply, "--",
                   "The first attached image is Image A and the second is Image B. Do not run any commands.\n\n" + prompt]
        try:
            result = subprocess.run(command, cwd=work, stdin=subprocess.DEVNULL, capture_output=True, text=True)
            if result.returncode:
                parser.exit(1, f"error: codex judge failed (exit {result.returncode}): {result.stderr.strip()}\n")
            with open(reply) as f:
                text = f.read()
            if not text.strip():
                parser.exit(1, "error: codex judge returned an empty reply\n")
        except OSError as error:
            parser.exit(1, f"error: codex judge failed: {error}\n")
    print(text, end="" if text.endswith("\n") else "\n")


if __name__ == "__main__":
    main()
