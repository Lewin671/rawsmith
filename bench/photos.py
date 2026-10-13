#!/usr/bin/env python3
"""Photo benchmark: render real RAW files several ways and have an LLM judge
blind pairwise comparisons.

Works on any folder of RAW files; nothing here is specific to one photo set.
Images are only ever written to the work directory you choose, which must be
outside this repository. Reports contain metrics and evaluation provenance,
never images, photo file names or private paths. Custom commands are published
only as SHA-256 hashes.

    bench/photos.py render --raws DIR --work DIR [--recipes DIR]
    bench/photos.py pairs  --work DIR
    bench/photos.py judge  --work DIR [--model MODEL] [--effort EFFORT] [--jobs N]
    bench/photos.py judge  --work DIR --judge-cmd CMD --model MODEL --name LABEL
    bench/photos.py report --work DIR [--json FILE]

Variants compared (see docs/benchmark.md for what each comparison shows):
  camera   the JPEG the camera embedded in the RAW (the manufacturer's rendering)
  system   the operating system's RAW converter (`sips`, macOS only)
  neutral  rawsmith with the default recipe
  auto     rawsmith after `rawsmith auto`
  ai       rawsmith with the recipe an AI assistant produced (--recipes DIR/<name>.json)

The default judge is bench/judge-codex.py, using gpt-6.1-sol at low reasoning
effort. Resume verifies configuration, exact prompts, task assignments and
image bytes; configuration changes and legacy caches require a new --name (or archiving the old file).
Custom commands must pin their actual model and effort and declare them with
--model and --effort.

Python 3.8+, standard library only.
"""
import argparse
import concurrent.futures
import hashlib
import json
import math
import os
import random
import re
import shlex
import shutil
import stat
import subprocess
import sys
import tempfile
import time

EDGE = 1400  # long edge of the images the judge sees
REPO = os.path.realpath(os.path.join(os.path.dirname(os.path.abspath(__file__)), ".."))
WORK_MARKER = ".rawsmith-benchmark.json"
WORK_FORMAT = {"format": "rawsmith-photo-benchmark", "version": 1}
JUDGE_FORMAT = {"format": "rawsmith-photo-verdicts", "version": 2}
DEFAULT_JUDGE = shlex.quote(os.path.join(REPO, "bench", "judge-codex.py"))
DEFAULT_MODEL = "gpt-6.1-sol"
DEFAULT_EFFORT = "low"
COMPARISONS = [("neutral", "camera"), ("auto", "camera"), ("auto", "system"), ("auto", "neutral"),
               ("ai", "auto"), ("ai", "camera"), ("ai", "system")]
# Deliberately bad recipes. A trustworthy judge must prefer the unspoiled picture.
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


def die(message):
    sys.exit(f"error: {message}")


def work_dir(path):
    if os.path.islink(os.path.abspath(path)):
        die("the work directory must not be a symbolic link")
    # Resolve symlinks first: a link that points into the repository is inside it.
    path = os.path.realpath(path)
    if os.path.commonpath([path, REPO]) == REPO:
        die("the work directory must be outside the repository, so rendered photos can never be committed")
    os.makedirs(path, exist_ok=True)
    marker = os.path.join(path, WORK_MARKER)
    contents = os.listdir(path)
    if WORK_MARKER not in contents:
        if contents:
            die("the work directory is not an empty or marked rawsmith benchmark directory; "
                "choose a new empty directory (existing files have been left alone)")
        with open(marker, "x") as f:
            json.dump(WORK_FORMAT, f)
    else:
        if not stat.S_ISREG(os.stat(marker, follow_symlinks=False).st_mode):
            die("the benchmark directory marker must be a regular file")
        try:
            owner = load(marker)
        except (ValueError, OSError) as error:
            die(f"invalid benchmark directory marker: {error}")
        if owner != WORK_FORMAT or type(owner.get("version")) is not int:
            die("unrecognised benchmark directory marker format or version; choose a new empty directory")
    # Even an owned directory must not redirect scratch writes or blind image
    # reads outside it. os.walk does not follow directory links.
    def unreadable(error):
        die(f"cannot inspect benchmark directory: {error}")
    for root, dirs, files in os.walk(path, onerror=unreadable):
        for name in dirs + files:
            if os.path.islink(os.path.join(root, name)):
                die("the benchmark work directory must not contain symbolic links")
    return path


def load(path, default=None):
    try:
        with open(path) as f:
            return json.load(f)
    except FileNotFoundError:
        return default


def save(path, value):
    # A fresh staging file also protects an explicitly chosen report path:
    # an existing .tmp name must never redirect writes to someone else's file.
    fd, staging = tempfile.mkstemp(prefix=".rawsmith-", dir=os.path.dirname(os.path.abspath(path)))
    try:
        with os.fdopen(fd, "w") as f:
            json.dump(value, f, indent=1)
        os.replace(staging, path)
    finally:
        if os.path.exists(staging):
            os.remove(staging)


def reset_tasks(work):
    """Remove generated tasks, retaining verdicts for fingerprinted resume."""
    tasks = os.path.join(work, "tasks.json")
    if os.path.exists(tasks):
        os.remove(tasks)
    shutil.rmtree(os.path.join(work, "blind"), ignore_errors=True)


def rawsmith(exe, *args):
    started = time.time()
    out = subprocess.run([exe, *args], capture_output=True, text=True)
    if out.returncode:
        raise RuntimeError(out.stderr.strip())
    return json.loads(out.stdout), time.time() - started


def cmd_render(args):
    work = work_dir(args.work)
    exe = args.rawsmith or os.path.join(REPO, "target", "release", "rawsmith")
    if not os.path.exists(exe):
        die(f"{exe} not found; run `cargo build --release` first")
    names = sorted(f for f in os.listdir(args.raws) if f.lower().endswith(".arw"))
    if not names:
        die(f"no .ARW files in {args.raws}")
    sips = shutil.which("sips")
    # sips renders RAW into Display P3; convert so every variant is plain sRGB.
    srgb = ["-m", "/System/Library/ColorSync/Profiles/sRGB Profile.icc"]
    rendering = rendering_identity(exe)
    reset_tasks(work)
    images = os.path.join(work, "images")
    shutil.rmtree(images, ignore_errors=True)
    os.makedirs(images)
    manifest = {"edge": EDGE, "photos": {}, "rendering": rendering,
                "camera_color_space": {"srgb": 0, "adobe_rgb_converted": 0, "skipped": 0}}
    for index, name in enumerate(names):
        raw = os.path.join(args.raws, name)
        # Photos are known by index only from here on.
        photo = f"p{index:03d}"
        entry = {"variants": {}, "seconds": {}}
        out = lambda variant: os.path.join(images, f"{photo}-{variant}.jpg")
        size = ["--size", str(EDGE), "--quality", "92"]

        scratch = os.path.join(work, "scratch.json")
        def render(variant, recipe, extra=()):
            save(scratch, recipe)
            report, seconds = rawsmith(exe, "render", raw, "--recipe", scratch, *extra, *size, "-o", out(variant))
            entry["variants"][variant] = report["stats"]
            entry["seconds"][variant] = round(seconds, 3)

        render("neutral", {})
        os.remove(scratch)
        report, seconds = rawsmith(exe, "auto", raw, "--recipe", scratch, *size, "-o", out("auto"))
        entry["variants"]["auto"], entry["seconds"]["auto"] = report["stats"], round(seconds, 3)
        entry["auto_recipe"] = {k: v for k, v in report["recipe"].items()}
        if args.recipes:
            recipe = os.path.join(args.recipes, os.path.splitext(name)[0] + ".json")
            if os.path.exists(recipe):
                report, _ = rawsmith(exe, "render", raw, "--recipe", recipe, *size, "-o", out("ai"))
                entry["variants"]["ai"] = report["stats"]
        if index % 3 == 0:
            # Controls spoil the auto rendering in one obvious way each.
            for control, settings in CONTROLS.items():
                try:
                    render(f"control-{control}", entry["auto_recipe"], [flag for s in settings for flag in ("--set", s)])
                except RuntimeError as error:
                    # e.g. a colour cast this photo's white balance cannot take
                    print(f"  control {control} skipped: {error}", flush=True)
        # compare applies RAW orientation and establishes/converts camera colour.
        # Never judge unmanaged pixels as if they were an sRGB reference.
        camera = {"status": "skipped", "reason": "camera rendering failed"}
        try:
            report, _ = rawsmith(exe, "compare", raw, "--panel", "camera",
                                 "--size", str(EDGE), "-o", out("camera"))
            panel = report["panels"][0]
            camera["color_declaration"] = panel["color_declaration"]
            camera["color_conversion"] = panel["color_conversion"]
            declared = panel["color_declaration"]["color_space"]
            if report["output_color_space"] == "sRGB" and panel["color_space"] == "sRGB":
                if declared == "sRGB" and panel["color_conversion"] is None:
                    camera = {**camera, "status": "srgb"}
                elif declared == "Adobe RGB (1998)" and panel["color_conversion"] == "Adobe RGB (1998) to sRGB":
                    camera = {**camera, "status": "adobe_rgb_converted"}
            if camera["status"] == "skipped":
                camera["reason"] = "camera colour space could not be established"
        except RuntimeError:
            pass
        if camera["status"] == "skipped":
            if os.path.exists(out("camera")):
                os.remove(out("camera"))
            print(f"{photo}: camera skipped: {camera['reason']}", flush=True)
        else:
            camera.pop("reason")
            entry["variants"]["camera"] = None
        entry["camera"] = camera
        manifest["camera_color_space"][camera["status"]] += 1
        if sips:
            started = time.time()
            subprocess.run([sips, *srgb, "-s", "format", "jpeg", "-s", "formatOptions", "92", "-Z", str(EDGE), raw, "--out", out("system")], capture_output=True, check=True)
            entry["seconds"]["system"] = round(time.time() - started, 3)
            entry["variants"]["system"] = None
        # Full-resolution export time, the number users feel: render, encode, write.
        save(scratch, {})
        export = os.path.join(work, "export.jpg")
        _, entry["seconds"]["full_resolution"] = rawsmith(exe, "render", raw, "--recipe", scratch, "-o", export)
        os.remove(export)
        entry["seconds"]["full_resolution"] = round(entry["seconds"]["full_resolution"], 3)
        manifest["photos"][photo] = entry
        print(f"{photo}: {', '.join(sorted(entry['variants']))}", flush=True)
    if os.path.exists(os.path.join(work, "scratch.json")):
        os.remove(os.path.join(work, "scratch.json"))
    save(os.path.join(work, "manifest.json"), manifest)


def cmd_pairs(args):
    work = work_dir(args.work)
    manifest = load(os.path.join(work, "manifest.json")) or die("run `render` first")
    rng = random.Random(args.seed)
    reset_tasks(work)
    blind = os.path.join(work, "blind")
    os.makedirs(blind)
    tasks = []
    def add(photo, kind, first, second):
        # Each pair is shown in both orders; file names reveal nothing.
        for order in ((first, second), (second, first)):
            task = f"t{len(tasks):04d}"
            for side, variant in zip("AB", order):
                shutil.copyfile(os.path.join(work, "images", f"{photo}-{variant}.jpg"), os.path.join(blind, f"{task}-{side}.jpg"))
            tasks.append({"task": task, "photo": photo, "kind": kind, "A": order[0], "B": order[1]})
    for photo, entry in manifest["photos"].items():
        have = entry["variants"]
        for first, second in COMPARISONS:
            if first in have and second in have:
                add(photo, "comparison", first, second)
        for control in CONTROLS:
            if f"control-{control}" in have:
                add(photo, "control", "auto", f"control-{control}")
        if list(manifest["photos"]).index(photo) % 4 == 0:
            add(photo, "identical", "auto", "auto")
    rng.shuffle(tasks)
    manifest["task_seed"] = args.seed
    save(os.path.join(work, "manifest.json"), manifest)
    save(os.path.join(work, "tasks.json"), tasks)
    print(f"{len(tasks)} judging tasks in {blind}")


def parse_verdict(text):
    """Extracts the judge's JSON object from its reply."""
    try:
        outer = json.loads(text)
        if isinstance(outer, dict) and "result" in outer:
            text = outer["result"]
    except ValueError:
        pass
    if not isinstance(text, str):
        raise ValueError("the judge's reply holds no text")
    start, end = text.find("{"), text.rfind("}")
    verdict = json.loads(text[start:end + 1])
    if not isinstance(verdict, dict):
        raise ValueError("the judge must reply with a JSON object")
    if verdict.get("winner") not in ("A", "B", "tie"):
        raise ValueError(f"unexpected winner {verdict.get('winner')!r}")
    return verdict


def digest(value):
    """Hash structured data unambiguously, independent of dictionary order."""
    return hashlib.sha256(json.dumps(value, sort_keys=True, separators=(",", ":")).encode()).hexdigest()


def text_digest(text):
    return hashlib.sha256(text.encode("utf-8")).hexdigest()


def file_digest(path):
    sha = hashlib.sha256()
    with open(path, "rb") as f:
        for block in iter(lambda: f.read(1024 * 1024), b""):
            sha.update(block)
    return sha.hexdigest()


def timestamp():
    return time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())


def rendering_identity(exe):
    try:
        version = subprocess.run([exe, "--version"], capture_output=True, text=True)
    except OSError as error:
        die(f"rawsmith --version failed: {error}")
    if version.returncode or not version.stdout.strip():
        die("rawsmith --version failed; rendering provenance cannot be recorded")
    try:
        commit = subprocess.run(["git", "-C", REPO, "rev-parse", "HEAD"], capture_output=True, text=True)
        revision = commit.stdout.strip() if commit.returncode == 0 else None
    except OSError:
        revision = None
    return {"rawsmith_version": version.stdout.strip(), "repository_commit": revision}


def task_inputs(work, task):
    # A task file must not redirect image reads outside the marked directory.
    if not re.fullmatch(r"t[0-9]+", task["task"]):
        raise ValueError("invalid blind task identifier")
    a, b = (os.path.join(work, "blind", f"{task['task']}-{side}.jpg") for side in "AB")
    prompt = PROMPT.format(a=a, b=b)
    return {"task": task, "prompt_sha256": text_digest(prompt),
            "images": [file_digest(a), file_digest(b)]}, prompt


def task_set_identity(manifest, tasks, inputs):
    ordered = sorted(tasks, key=lambda t: t["task"])
    return {"seed": manifest.get("task_seed"), "tasks": len(tasks),
            "tasks_sha256": digest(ordered),
            "blind_images_sha256": digest([(t["task"], inputs.get(t["task"], {}).get("images")) for t in ordered])}


def current_inputs(work, tasks, strict=False):
    inputs = {}
    for task in tasks:
        try:
            inputs[task["task"]], _ = task_inputs(work, task)
        except (OSError, ValueError, KeyError) as error:
            if strict:
                die(f"cannot fingerprint blind task: {error}; rerun `pairs`")
    return inputs


def verified_file(stored):
    return (isinstance(stored, dict) and stored.get("format") == JUDGE_FORMAT["format"]
            and type(stored.get("version")) is int and stored["version"] == JUDGE_FORMAT["version"]
            and isinstance(stored.get("configuration"), dict)
            and all(isinstance(stored["configuration"].get(key), str) for key in ("judge_command", "model", "effort", "prompt_sha256"))
            and isinstance(stored["configuration"].get("harness"), dict)
            and isinstance(stored.get("runs"), list)
            and stored.get("configuration_sha256") == digest(stored["configuration"])
            and isinstance(stored.get("verdicts"), dict))


def valid_verdict(verdict, config_hash, inputs):
    return (inputs is not None and isinstance(verdict, dict)
            and verdict.get("winner") in ("A", "B", "tie")
            and verdict.get("fingerprint") == digest({"configuration": config_hash, "inputs": inputs}))


def cmd_judge(args):
    work = work_dir(args.work)
    tasks = load(os.path.join(work, "tasks.json")) or die("run `pairs` first")
    if not re.fullmatch(r"[A-Za-z0-9_-]+", args.name):
        die("--name must contain only letters, digits, underscores or hyphens")
    if args.jobs < 1:
        die("--jobs must be at least 1")
    if args.model is not None and not args.model.strip():
        die("--model must not be empty")
    if not args.effort.strip():
        die("--effort must not be empty")
    builtin = args.judge_cmd == DEFAULT_JUDGE
    model = args.model or (DEFAULT_MODEL if builtin else None)
    if not model:
        die("custom --judge-cmd requires --model; pin that same model in the command")
    command = args.judge_cmd + (" --model " + shlex.quote(model) + " --effort " + shlex.quote(args.effort) if builtin else "")
    configuration = {"judge_command": command, "model": model, "effort": args.effort,
                     "prompt_sha256": text_digest(PROMPT), "harness": JUDGE_FORMAT,
                     "adapter_sha256": file_digest(os.path.join(REPO, "bench", "judge-codex.py")) if builtin else None,
                     "builtin_adapter": builtin}
    config_hash = digest(configuration)
    verdicts_path = os.path.join(work, f"verdicts-{args.name}.json")
    stored = load_verdict_file(verdicts_path)
    if os.path.exists(verdicts_path):
        if not verified_file(stored):
            die("existing verdict file is legacy or unverifiable and cannot be reused; "
                "use a new --name or archive the old verdict file first (left unchanged)")
        if stored["configuration"] != configuration:
            die("judge command, model, effort, prompt or harness configuration changed; "
                "use a new --name or archive the old verdict file first (left unchanged)")
    else:
        stored = {**JUDGE_FORMAT, "configuration": configuration, "configuration_sha256": config_hash,
                  "verdicts": {}, "runs": []}
    inputs = current_inputs(work, tasks, strict=True)
    manifest = load(os.path.join(work, "manifest.json"), {})
    identity = task_set_identity(manifest, tasks, inputs)
    verdicts = stored["verdicts"]
    todo = [t for t in tasks if not valid_verdict(verdicts.get(t["task"]), config_hash, inputs[t["task"]])]
    print(f"{len(todo)} of {len(tasks)} tasks to judge with: {command}", flush=True)
    if not todo:
        return
    print("Changed/unverifiable task verdicts will be replaced; unchanged verdicts are retained.", flush=True)
    run = {"started_at": timestamp(), "finished_at": None, "task_set": identity}
    stored["runs"].append(run)
    stored["task_set"] = identity
    save(verdicts_path, stored)

    def judge(task):
        initial = inputs[task["task"]]
        for attempt in range(3):
            out = None
            try:
                latest, prompt = task_inputs(work, task)
                if latest != initial:
                    raise ValueError("task images changed before judging; rerun `judge`")
                out = subprocess.run(command, shell=True, input=prompt, capture_output=True, text=True, cwd=os.path.join(work, "blind"))
                if out.returncode:
                    raise ValueError(f"judge exited with status {out.returncode}")
                verdict = parse_verdict(out.stdout)
                latest, _ = task_inputs(work, task)
                if latest != initial:
                    raise ValueError("task images changed during judging; rerun `judge`")
                verdict.update(fingerprint=digest({"configuration": config_hash, "inputs": initial}),
                               judged_at=timestamp(), run=len(stored["runs"]) - 1)
                return task["task"], verdict
            except (OSError, ValueError, KeyError) as error:
                last = str(error) + (f": {out.stdout[-300:]} {out.stderr[-300:]}" if out else "")
        return task["task"], {"winner": None, "error": last}

    def persist(result):
        task, verdict = result
        verdicts[task] = verdict
        save(verdicts_path, stored)
        print(f"[{len(todo) - len(pending)}/{len(todo)}] {task}: {verdict.get('winner')}", flush=True)

    with concurrent.futures.ThreadPoolExecutor(args.jobs) as pool:
        pending = {pool.submit(judge, task) for task in todo}
        try:
            for future in concurrent.futures.as_completed(pending):
                pending.remove(future)
                persist(future.result())
        except KeyboardInterrupt:
            # Save already finished tasks even if the interrupted future was
            # yielded first. Unstarted tasks need no retry until resume.
            for future in list(pending):
                pending.remove(future)
                if future.done() and not future.cancelled() and future.exception() is None:
                    persist(future.result())
                else:
                    future.cancel()
            raise
    run["finished_at"] = timestamp()
    save(verdicts_path, stored)
    if any(not valid_verdict(verdicts.get(t["task"]), config_hash, inputs[t["task"]]) for t in tasks):
        die("some tasks failed; valid verdicts were saved, rerun `judge` to retry")


def load_verdict_file(path):
    try:
        return load(path)
    except ValueError:
        # An unreadable cache is no evidence, and must not be overwritten.
        return None


def report_verdicts(stored, inputs, task_ids):
    counts = {"valid": 0, "stale": 0, "failed": 0, "unverifiable": 0, "orphaned": 0}
    if not verified_file(stored):
        entries = stored.get("verdicts", stored) if isinstance(stored, dict) else stored
        counts["unverifiable"] = len(entries) if isinstance(entries, dict) else 1
        return {}, None, counts
    configuration = stored["configuration"]
    current = (configuration.get("prompt_sha256") == text_digest(PROMPT)
               and configuration.get("harness") == JUDGE_FORMAT)
    if configuration.get("builtin_adapter"):
        try:
            current = current and configuration.get("adapter_sha256") == file_digest(os.path.join(REPO, "bench", "judge-codex.py"))
        except OSError:
            current = False
    valid = {}
    for task, verdict in stored["verdicts"].items():
        if task not in task_ids:
            counts["orphaned"] += 1
        elif task not in inputs:
            counts["stale"] += 1
        elif not isinstance(verdict, dict) or verdict.get("winner") not in ("A", "B", "tie"):
            counts["failed"] += 1
        elif not verdict.get("fingerprint"):
            counts["unverifiable"] += 1
        elif current and valid_verdict(verdict, stored["configuration_sha256"], inputs[task]):
            valid[task] = verdict
            counts["valid"] += 1
        else:
            counts["stale"] += 1
    # Arbitrary shell commands may contain private paths or names. Publish only
    # their exact hash; the local verdict file retains the literal command.
    public = {"judge_command": {"sha256": text_digest(configuration["judge_command"]),
                                "adapter": "bench/judge-codex.py" if configuration.get("builtin_adapter") else "custom"},
              "model": configuration["model"], "effort": configuration["effort"], "prompt_sha256": configuration["prompt_sha256"],
              "harness": configuration["harness"], "adapter_sha256": configuration.get("adapter_sha256"),
              "task_set": stored.get("task_set"), "runs": stored.get("runs", [])}
    return valid, public, counts


def wilson(wins, n):
    """95% Wilson score interval for a proportion."""
    if n == 0:
        return (0.0, 1.0)
    z, p = 1.96, wins / n
    centre = (p + z * z / (2 * n)) / (1 + z * z / n)
    half = z * math.sqrt(p * (1 - p) / n + z * z / (4 * n * n)) / (1 + z * z / n)
    return (max(0.0, centre - half), min(1.0, centre + half))


def summarise(tasks, verdicts):
    """Aggregates one judge's verdicts. The photo is the unit of analysis: the
    two orderings of a pair are combined into one outcome per photo, and a
    pair counts only when both orderings were judged."""
    by_pair = {}
    for task in tasks:
        verdict = verdicts.get(task["task"], {}).get("winner")
        chosen = None if verdict is None else "tie" if verdict == "tie" else task[verdict]
        by_pair.setdefault((task["kind"], task["photo"], *sorted([task["A"], task["B"]])), []).append((task, chosen, verdict))
    comparisons, controls = {}, {"correct": 0, "total": 0}
    consistent = complete = incomplete = 0
    position = {"A": 0, "B": 0, "tie": 0}
    identical = {"tie": 0, "total": 0}
    for (kind, photo, first, second), votes in by_pair.items():
        if len(votes) != 2 or any(chosen is None for _, chosen, _ in votes):
            incomplete += 1
            continue
        complete += 1
        for _, _, verdict in votes:
            position[verdict] += 1
        if kind == "identical":
            identical["total"] += 2
            identical["tie"] += sum(1 for _, chosen, _ in votes if chosen == "tie")
            continue
        consistent += votes[0][1] == votes[1][1]
        task = votes[0][0]
        if kind == "control":
            controls["total"] += 2
            controls["correct"] += sum(1 for _, chosen, _ in votes if chosen == "auto")
            continue
        for candidate, reference in COMPARISONS:
            if {candidate, reference} == {task["A"], task["B"]}:
                # +1 / 0 / -1 per ordering for the candidate, averaged.
                score = sum((chosen == candidate) - (chosen == reference) for _, chosen, _ in votes) / 2
                comparisons.setdefault(f"{candidate} vs {reference}", []).append(score)
    table = {}
    for candidate, reference in COMPARISONS:
        name = f"{candidate} vs {reference}"
        scores = comparisons.get(name)
        if not scores:
            continue
        wins, losses = sum(s > 0 for s in scores), sum(s < 0 for s in scores)
        decided = wins + losses
        low, high = wilson(wins, decided)
        table[name] = {
            "photos": len(scores), "wins": wins, "losses": losses, "ties": len(scores) - decided,
            "win_rate_of_decided": round(wins / decided, 3) if decided else None,
            "ci95": [round(low, 3), round(high, 3)],
            "mean_score": round(sum(scores) / len(scores), 3),
        }
    decided_pairs = complete - identical["total"] // 2
    return {
        "comparisons": table,
        "judge_checks": {
            "control_accuracy": round(controls["correct"] / controls["total"], 3) if controls["total"] else None,
            "control_votes": controls["total"],
            "order_consistency": round(consistent / decided_pairs, 3) if decided_pairs else None,
            "identical_pairs_called_tie": f"{identical['tie']}/{identical['total']}",
            "position_votes": position,
            "pairs_judged": complete,
            "pairs_incomplete": incomplete,
        },
    }


def cmd_report(args):
    work = work_dir(args.work)
    manifest = load(os.path.join(work, "manifest.json")) or die("run `render` first")
    tasks = load(os.path.join(work, "tasks.json"), [])
    photos = manifest["photos"].values()
    mean = lambda values: round(sum(values) / len(values), 3) if values else None
    seconds = {key: mean([p["seconds"][key] for p in photos if key in p["seconds"]]) for key in ("neutral", "auto", "system", "full_resolution")}
    clipping = {v: mean([p["variants"][v]["clipped_highlights_pct"] for p in photos if p["variants"].get(v)]) for v in ("neutral", "auto", "ai")}
    report = {"photos": len(manifest["photos"]), "preview_edge_px": manifest["edge"], "camera_color_space": manifest.get("camera_color_space"), "mean_seconds": seconds, "mean_clipped_highlights_pct": clipping, "rendering": manifest.get("rendering", {"rawsmith_version": None, "repository_commit": None}), "judges": {}}
    inputs = current_inputs(work, tasks)
    report["task_set"] = task_set_identity(manifest, tasks, inputs)
    for file in sorted(os.listdir(work)):
        if file.startswith("verdicts-") and file.endswith(".json"):
            verdicts, evaluation, counts = report_verdicts(load_verdict_file(os.path.join(work, file)), inputs, {t["task"] for t in tasks})
            summary = summarise(tasks, verdicts)
            summary.update(evaluation=evaluation, verdict_validation=counts)
            report["judges"][file[9:-5]] = summary
    if args.json:
        save(os.path.abspath(args.json), report)
    print(f"Photos: {report['photos']}   mean seconds: {seconds}")
    camera = report["camera_color_space"]
    if camera is None:
        print("Camera JPEG colour: unknown (older manifest)")
    else:
        print(f"Camera JPEG colour: {camera['srgb']} sRGB, "
              f"{camera['adobe_rgb_converted']} converted from Adobe RGB, {camera['skipped']} skipped")
    for judge, summary in report["judges"].items():
        checks = summary["judge_checks"]
        evaluation = summary["evaluation"]
        if evaluation:
            print(f"Judge configuration: model {evaluation['model']}, effort {evaluation['effort']}")
        print(f"\nVerdict validation: {summary['verdict_validation']} (unverifiable/stale verdicts excluded)")
        print(f"\nJudge: {judge}   control accuracy {checks['control_accuracy']} over {checks['control_votes']} votes, "
              f"order consistency {checks['order_consistency']}, identical pairs called tie {checks['identical_pairs_called_tie']}, "
              f"position votes {checks['position_votes']}, pairs judged {checks['pairs_judged']}"
              + (f"  ** {checks['pairs_incomplete']} pairs incomplete and excluded; rerun `judge` **" if checks["pairs_incomplete"] else ""))
        print(f"{'comparison':<20} {'photos':>6} {'wins':>5} {'losses':>6} {'ties':>5} {'win rate':>9}  95% CI")
        for name, row in summary["comparisons"].items():
            rate = "-" if row["win_rate_of_decided"] is None else f"{row['win_rate_of_decided']:.0%}"
            print(f"{name:<20} {row['photos']:>6} {row['wins']:>5} {row['losses']:>6} {row['ties']:>5} {rate:>9}  {row['ci95'][0]:.0%}-{row['ci95'][1]:.0%}")


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest="command", required=True)
    p = sub.add_parser("render", help="render every variant of every RAW")
    p.add_argument("--raws", required=True)
    p.add_argument("--work", required=True)
    p.add_argument("--recipes", help="folder of AI-made recipes named <raw name>.json")
    p.add_argument("--rawsmith", help="path to the rawsmith binary")
    p.set_defaults(run=cmd_render)
    p = sub.add_parser("pairs", help="build the blinded, order-swapped judging tasks")
    p.add_argument("--work", required=True)
    p.add_argument("--seed", type=int, default=1)
    p.set_defaults(run=cmd_pairs)
    p = sub.add_parser("judge", help="run an LLM judge over the tasks (resumable)",
                       description="Resume verifies prompts, assignments and image bytes. Configuration changes or legacy verdict files require a new --name or archiving the old file.")
    p.add_argument("--work", required=True)
    p.add_argument("--name", default="codex", help="verdict label (default: codex); use a new label when configuration changes")
    p.add_argument("--judge-cmd", default=DEFAULT_JUDGE,
                   help="shell command reading the prompt on stdin and printing a verdict (default: bench/judge-codex.py); custom commands must pin model and effort")
    p.add_argument("--model", help="actual judge model (default adapter: gpt-6.1-sol); required for custom commands")
    p.add_argument("--effort", default=DEFAULT_EFFORT, help="actual judge reasoning effort (default: low); custom commands must pin the same effort; supported levels depend on the model")
    p.add_argument("--jobs", type=int, default=4)
    p.set_defaults(run=cmd_judge)
    p = sub.add_parser("report", help="print validated results and provenance; no private photo paths")
    p.add_argument("--work", required=True)
    p.add_argument("--json", help="also write the report as JSON to this file")
    p.set_defaults(run=cmd_report)
    args = parser.parse_args()
    args.run(args)


if __name__ == "__main__":
    main()
