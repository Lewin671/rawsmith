#!/usr/bin/env python3
"""Photo benchmark: render real RAW files several ways and have an LLM judge
blind pairwise comparisons.

Works on any folder of RAW files; nothing here is specific to one photo set.
Images are only ever written to the work directory you choose, which must be
outside this repository. The report contains numbers only, never images,
file names or paths.

    bench/photos.py render --raws DIR --work DIR [--recipes DIR]
    bench/photos.py pairs  --work DIR
    bench/photos.py judge  --work DIR [--judge-cmd CMD] [--jobs N]
    bench/photos.py report --work DIR [--json FILE]

Variants compared (see docs/benchmark.md for what each comparison shows):
  camera   the JPEG the camera embedded in the RAW (the manufacturer's rendering)
  system   the operating system's RAW converter (`sips`, macOS only)
  neutral  rawsmith with the default recipe
  auto     rawsmith after `rawsmith auto`
  ai       rawsmith with the recipe an AI assistant produced (--recipes DIR/<name>.json)

Python 3.8+, standard library only.
"""
import argparse
import concurrent.futures
import json
import math
import os
import random
import shutil
import subprocess
import sys
import time

EDGE = 1400  # long edge of the images the judge sees
REPO = os.path.realpath(os.path.join(os.path.dirname(os.path.abspath(__file__)), ".."))
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
    # Resolve symlinks first: a link that points into the repository is inside it.
    path = os.path.realpath(path)
    if os.path.commonpath([path, REPO]) == REPO:
        die("the work directory must be outside the repository, so rendered photos can never be committed")
    os.makedirs(path, exist_ok=True)
    return path


def load(path, default=None):
    try:
        with open(path) as f:
            return json.load(f)
    except FileNotFoundError:
        return default


def save(path, value):
    with open(path + ".tmp", "w") as f:
        json.dump(value, f, indent=1)
    os.replace(path + ".tmp", path)


def forget_judgements(work):
    """Verdicts describe specific images in a specific order; once either
    changes they must not be reused."""
    for file in os.listdir(work):
        if file == "tasks.json" or (file.startswith("verdicts-") and file.endswith(".json")):
            os.remove(os.path.join(work, file))
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
    forget_judgements(work)
    images = os.path.join(work, "images")
    shutil.rmtree(images, ignore_errors=True)
    os.makedirs(images)
    manifest = {"edge": EDGE, "photos": {}}
    for index, name in enumerate(names):
        raw = os.path.join(args.raws, name)
        # Photos are known by index only from here on.
        photo = f"p{index:03d}"
        entry = {"variants": {}, "seconds": {}}
        out = lambda variant: os.path.join(images, f"{photo}-{variant}.jpg")
        size = ["--size", str(EDGE), "--quality", "92"]
        info, _ = rawsmith(exe, "inspect", raw)
        upright = info["camera"]["orientation"] in (1, None)

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
                render(f"control-{control}", entry["auto_recipe"], [flag for s in settings for flag in ("--set", s)])
        if sips:
            # The embedded JPEG is stored unrotated, the RAW engine applies orientation itself.
            embedded = os.path.join(work, "embedded.jpg")
            rawsmith(exe, "extract-jpeg", raw, "-o", embedded)
            rotate = [] if upright else ["-r", {8: "270", 6: "90", 3: "180"}[info["camera"]["orientation"]]]
            subprocess.run([sips, *rotate, *srgb, "-Z", str(EDGE), "-s", "formatOptions", "92", embedded, "--out", out("camera")], capture_output=True, check=True)
            started = time.time()
            subprocess.run([sips, *srgb, "-s", "format", "jpeg", "-s", "formatOptions", "92", "-Z", str(EDGE), raw, "--out", out("system")], capture_output=True, check=True)
            entry["seconds"]["system"] = round(time.time() - started, 3)
            entry["variants"]["camera"] = entry["variants"]["system"] = None
            os.remove(embedded)
        # Full-resolution render time, the number users feel on export.
        save(scratch, {})
        _, entry["seconds"]["full_resolution"] = rawsmith(exe, "render", raw, "--recipe", scratch)
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
    forget_judgements(work)
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
    start, end = text.find("{"), text.rfind("}")
    verdict = json.loads(text[start:end + 1])
    if verdict.get("winner") not in ("A", "B", "tie"):
        raise ValueError(f"unexpected winner {verdict.get('winner')!r}")
    return verdict


def cmd_judge(args):
    work = work_dir(args.work)
    tasks = load(os.path.join(work, "tasks.json")) or die("run `pairs` first")
    verdicts_path = os.path.join(work, f"verdicts-{args.name}.json")
    verdicts = load(verdicts_path, {})
    # A failed attempt (no winner) is not a verdict: try it again.
    todo = [t for t in tasks if verdicts.get(t["task"], {}).get("winner") is None]
    print(f"{len(todo)} of {len(tasks)} tasks to judge with: {args.judge_cmd}", flush=True)

    def judge(task):
        a, b = (os.path.join(work, "blind", f"{task['task']}-{side}.jpg") for side in "AB")
        prompt = PROMPT.format(a=a, b=b)
        for attempt in range(3):
            out = subprocess.run(args.judge_cmd, shell=True, input=prompt, capture_output=True, text=True, cwd=os.path.join(work, "blind"))
            try:
                return task["task"], parse_verdict(out.stdout)
            except (ValueError, KeyError) as error:
                last = f"{error}: {out.stdout[-300:]} {out.stderr[-300:]}"
        return task["task"], {"winner": None, "error": last}

    with concurrent.futures.ThreadPoolExecutor(args.jobs) as pool:
        for done, (task, verdict) in enumerate(pool.map(judge, todo), 1):
            verdicts[task] = verdict
            save(verdicts_path, verdicts)
            print(f"[{done}/{len(todo)}] {task}: {verdict.get('winner')}", flush=True)


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
    report = {"photos": len(manifest["photos"]), "preview_edge_px": manifest["edge"], "mean_seconds": seconds, "mean_clipped_highlights_pct": clipping, "judges": {}}
    for file in sorted(os.listdir(work)):
        if file.startswith("verdicts-") and file.endswith(".json"):
            report["judges"][file[9:-5]] = summarise(tasks, load(os.path.join(work, file)))
    if args.json:
        save(os.path.abspath(args.json), report)
    print(f"Photos: {report['photos']}   mean seconds: {seconds}")
    for judge, summary in report["judges"].items():
        checks = summary["judge_checks"]
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
    p = sub.add_parser("judge", help="run an LLM judge over the tasks (resumable)")
    p.add_argument("--work", required=True)
    p.add_argument("--name", default="claude", help="label for this judge's verdict file")
    p.add_argument("--judge-cmd", default="claude -p --allowedTools Read --output-format json",
                   help="shell command that reads the prompt on stdin, can open the two image paths in it, and prints the reply")
    p.add_argument("--jobs", type=int, default=4)
    p.set_defaults(run=cmd_judge)
    p = sub.add_parser("report", help="print the results; numbers only")
    p.add_argument("--work", required=True)
    p.add_argument("--json", help="also write the report as JSON to this file")
    p.set_defaults(run=cmd_report)
    args = parser.parse_args()
    args.run(args)


if __name__ == "__main__":
    main()
