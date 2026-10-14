#!/usr/bin/env python3
"""Conservatively select an existing RAW edit; cloud judging requires --allow-cloud."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile

MODEL = "gpt-6.1-sol"
EFFORT = "low"
CONTROLS = {
    "underexposed": ["exposure-=2.5"],
    "colour_cast": ["tint=120"],
    "oversaturated": ["saturation=100", "vibrance=100"],
    "flat": ["contrast=-100", "blacks=100"],
}
PROMPT = """You are an experienced photo editor judging two renderings of the same photograph, made from the same RAW file by two different developing processes.

Image A: {a}
Image B: {b}

Look at both images. Decide which is the better finished photograph of this scene, as you would when choosing which version to deliver. Consider:
- exposure and tonal balance: is the subject readable, are highlights and shadows handled well;
- colour: natural and pleasing, no unwanted cast, neither dull nor garish;
- depth and clarity appropriate to the scene; mood preserved (fog should stay fog, a silhouette can stay a silhouette, snow should look like snow);
- defects: halos, banding, blotchy noise, crushed or blown areas, artificial look.

Judge only what you see. Do not guess which process made which image, and do not favour either position. If they are equally good, or differ only in ways that do not matter, answer "tie".

Reply with one JSON object and nothing else:
{{"winner": "A" | "B" | "tie", "confidence": 1 | 2 | 3, "reason": "<one sentence>"}}"""


def executable(name, fallback=None):
    found = shutil.which(name)
    if found:
        return found
    if fallback and fallback.is_file():
        return str(fallback)
    raise ValueError(f"executable not found: {name}")


def render(engine, raw, recipe, output, preview=False, settings=()):
    command = [engine, "render", str(raw), "--recipe", str(recipe),
               "--quality", "92", "-o", str(output)]
    if preview:
        command += ["--size", "1400"]
    for setting in settings:
        command += ["--set", setting]
    result = subprocess.run(command, capture_output=True, text=True)
    if result.returncode or not output.is_file() or output.stat().st_size == 0:
        raise ValueError(f"rawsmith export failed (exit {result.returncode}): {result.stderr.strip()}")


def judge(codex, a, b, labels):
    with tempfile.TemporaryDirectory(prefix="rawsmith-judge-") as directory:
        reply = Path(directory) / "reply.json"
        # Neutral attachment names hide which recipe made each image.
        first = Path(directory) / "image-A.jpg"
        second = Path(directory) / "image-B.jpg"
        shutil.copyfile(a, first)
        shutil.copyfile(b, second)
        command = [codex, "exec", "-m", MODEL, "-c", 'model_reasoning_effort="low"',
                   "-s", "read-only", "--ephemeral", "--skip-git-repo-check", "--ignore-user-config",
                   "--disable", "hooks", "--disable", "apps", "--disable", "plugins", "--disable", "memories",
                   "-i", str(first), "-i", str(second), "-o", str(reply), "--",
                   "The first attached image is Image A and the second is Image B. Do not run any commands.\n\n"
                   + PROMPT.format(a=first, b=second)]
        try:
            result = subprocess.run(command, cwd=directory, stdin=subprocess.DEVNULL,
                                    capture_output=True, text=True, timeout=300)
            if result.returncode:
                raise ValueError(f"Codex judge failed (exit {result.returncode})")
            vote = json.loads(reply.read_text())
            if (not isinstance(vote, dict) or vote.get("winner") not in ("A", "B", "tie")
                    or type(vote.get("confidence")) is not int or vote["confidence"] not in (1, 2, 3)
                    or not isinstance(vote.get("reason"), str) or not vote["reason"].strip()):
                raise ValueError("invalid judge JSON")
            return {"winner": "tie" if vote["winner"] == "tie" else labels[vote["winner"]],
                    "confidence": vote["confidence"], "reason": vote["reason"], "order": labels}
        except (OSError, ValueError, subprocess.TimeoutExpired) as error:
            return {"winner": "error", "confidence": 0, "reason": str(error), "order": labels}


def pair(codex, first, second, names):
    return [judge(codex, first, second, dict(zip("AB", names))),
            judge(codex, second, first, dict(zip("AB", reversed(names))))]


def wins(votes, expected):
    return len(votes) == 2 and all(v["winner"] == expected and v["confidence"] >= 2 for v in votes)


def check_paths(inputs, outputs):
    for source in inputs:
        if not source.is_file():
            raise ValueError(f"input is not a file: {source}")
    for target in outputs:
        if target.exists() or target.is_symlink():
            raise ValueError(f"refusing existing output: {target}")
        if not target.parent.is_dir():
            raise ValueError(f"output parent does not exist: {target.parent}")
        if any(target.resolve() == source.resolve() for source in inputs):
            raise ValueError("output aliases an input")
    if outputs[0].resolve() == outputs[1].resolve():
        raise ValueError("output and recipe must be different files")


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("raw", type=Path)
    for name in ("baseline", "candidate", "output", "recipe"):
        parser.add_argument("--" + name, type=Path, required=True)
    parser.add_argument("--rawsmith", help="rawsmith executable (default: PATH, then ~/.local/bin/rawsmith)")
    parser.add_argument("--allow-cloud", action="store_true", help="authorize sending previews to the Codex cloud judge")
    args = parser.parse_args(argv)
    if not args.allow_cloud:
        parser.error("--allow-cloud is required; no images sent and no results written")
    try:
        inputs = [p.absolute() for p in (args.raw, args.baseline, args.candidate)]
        output, recipe = (p.absolute() for p in (args.output, args.recipe))
        check_paths(inputs, [output, recipe])
        if output.suffix.lower() not in (".jpg", ".jpeg"):
            raise ValueError("--output must have a .jpg or .jpeg extension")
        engine = executable(args.rawsmith or "rawsmith", None if args.rawsmith else Path.home() / ".local/bin/rawsmith")
        report = {"model": MODEL, "effort": EFFORT, "choice": "baseline", "selection": [],
                  "acceptance": [], "controls": {}, "reason": "candidate did not pass selection",
                  "limitation": "Cloud judgments cannot guarantee no regression on unknown photographs."}
        # Snapshot bytes before rendering; export the exact selected recipe bytes.
        snapshots = {name: source.read_bytes() for name, source in zip(("baseline", "candidate"), inputs[1:])}
        report["recipe_sha256"] = {name: hashlib.sha256(data).hexdigest() for name, data in snapshots.items()}
        with tempfile.TemporaryDirectory(prefix="rawsmith-select-") as directory:
            root = Path(directory)
            recipes = {}
            images = {}
            for name, data in snapshots.items():
                recipes[name] = root / (name + ".json")
                recipes[name].write_bytes(data)
                images[name] = root / (name + ".jpg")
            render(engine, inputs[0], recipes["baseline"], images["baseline"], preview=True)
            try:
                render(engine, inputs[0], recipes["candidate"], images["candidate"], preview=True)
            except (OSError, ValueError, subprocess.SubprocessError) as error:
                report["reason"] = f"candidate preview failed; retained baseline: {error}"
                report["candidate_render_error"] = str(error)
            else:
                try:
                    codex = executable("codex")
                except ValueError as error:
                    report["reason"] = str(error) + "; retained baseline"
                else:
                    report["selection"] = pair(codex, images["candidate"], images["baseline"], ("candidate", "baseline"))
                    if wins(report["selection"], "candidate"):
                        report["acceptance"] = pair(codex, images["candidate"], images["baseline"], ("candidate", "baseline"))
                        controls_ok = True
                        # Four identical votes across both legitimate renderings.
                        for name in ("baseline", "candidate"):
                            votes = pair(codex, images[name], images[name], (name, name))
                            report["controls"]["identical_" + name] = votes
                            controls_ok = controls_ok and all(v["winner"] == "tie" for v in votes)
                        for name, settings in CONTROLS.items():
                            spoiled = root / (name + ".jpg")
                            try:
                                render(engine, inputs[0], recipes["baseline"], spoiled, preview=True, settings=settings)
                            except (OSError, ValueError, subprocess.SubprocessError) as error:
                                report["controls"][name] = {"render_error": str(error), "votes": []}
                                controls_ok = False
                            else:
                                votes = pair(codex, images["baseline"], spoiled, ("baseline", "spoiled"))
                                report["controls"][name] = {"votes": votes}
                                controls_ok = controls_ok and wins(votes, "baseline")
                        report["controls_passed"] = controls_ok
                        if wins(report["acceptance"], "candidate") and controls_ok:
                            report["choice"] = "candidate"
                            report["reason"] = "candidate passed both blind selection orders, fresh acceptance and controls"
                        else:
                            report["reason"] = "fresh acceptance or controls failed; retained baseline"
            # Refuse existing destinations even if created while the judges ran.
            check_paths(inputs, [output, recipe])
            with tempfile.TemporaryDirectory(prefix=".rawsmith-export-", dir=output.parent) as export_directory:
                staged_image = Path(export_directory) / "selected.jpg"
                render(engine, inputs[0], recipes[report["choice"]], staged_image)
                with tempfile.TemporaryDirectory(prefix=".rawsmith-recipe-", dir=recipe.parent) as recipe_directory:
                    staged_recipe = Path(recipe_directory) / "selected.json"
                    staged_recipe.write_bytes(snapshots[report["choice"]])
                    os.link(staged_image, output)  # Exclusive creation, including racing writers.
                    try:
                        os.link(staged_recipe, recipe)
                    except BaseException:
                        # Only remove our own new image, never a replacement by another writer.
                        if output.exists() and os.path.samefile(output, staged_image):
                            output.unlink()
                        raise
        print(json.dumps(report, ensure_ascii=False))
        return 0
    except (OSError, ValueError, subprocess.SubprocessError) as error:
        print(f"error: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
