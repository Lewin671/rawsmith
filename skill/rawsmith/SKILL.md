---
name: rawsmith
description: >-
  Develop and edit camera RAW photos (Sony ARW) into finished pictures with
  the `rawsmith` CLI: exposure, white balance, tone, colour, crop and
  export. Use when the user asks to edit, develop, grade, fix or export a
  RAW photo, or to batch-process a folder of RAW files. Requires the
  `rawsmith` binary on PATH and the ability to view image files.
---

# Editing RAW photos with rawsmith

`rawsmith` turns a RAW file plus a small JSON *recipe* into a picture. The
RAW is never modified. You edit by changing recipe parameters, rendering a
preview, **looking at it**, and repeating. Every command prints JSON.

## The loop

1. **Inspect** — `rawsmith inspect photo.ARW`
   Camera settings, as-shot white balance, and how the sensor was exposed
   (`saturated_pct`: data that is gone for good; `headroom_stops`: unused
   highlight room).

2. **Start from auto** — `rawsmith auto photo.ARW -o preview.jpg --size 1400`
   Analyses the scene, saves a starting recipe next to the RAW
   (`photo.ARW.rawsmith.json`) and explains each decision. Auto is a
   competent, conservative starting point, not the finished edit.

3. **Look** at `preview.jpg` (open the file — numbers alone are not enough)
   and read `stats` in the output. Decide what the picture needs. Name the
   problems before touching parameters: "foreground too dark", "sky washed
   out", "snow looks grey-blue", "flat and hazy".

4. **Change a few related parameters** —
   `rawsmith edit photo.ARW --set shadows=35 --set highlights=-40 -o preview.jpg --size 1400`
   `path=value` sets a value; `path+=value` / `path-=value` nudges the
   current one. The recipe is saved, a preview rendered, stats returned.
   Fix the biggest problem first: exposure and white balance, then tone
   (highlights, shadows, contrast), then colour, then local finishing.

5. **Compare with the previous preview** (write each round to a new file
   name while you are deciding). Keep the change only if the picture is
   better. If not, set the values back. Stop when another round would not
   visibly improve the picture — usually after 3 to 6 rounds. More rounds
   tend to over-process.

6. **Check detail at 100%** before delivering —
   `rawsmith render photo.ARW --region 0.4,0.4,0.6,0.6 -o detail.jpg`
   Look for noise in lifted shadows, harsh sharpening, halos on edges.

7. **Export** — `rawsmith render photo.ARW -o final.jpg` (full resolution;
   `.png` gives 16-bit). Report what you changed and why, in plain words.

`rawsmith schema` lists every parameter with range and meaning. Use
`rawsmith render ... --set k=v` to try something without saving it, and
`rawsmith edit ... --reset` to start over. `--recipe FILE` keeps several
versions of one photo.

## Parameters that matter most

| Goal | Parameters |
|---|---|
| Overall brightness | `exposure` (stops) |
| Colour cast, warmth | `temperature` (K, higher = warmer), `tint` (+ magenta, − green) |
| Blown-looking sky, harsh snow | `highlights` negative, `whites` negative |
| Dark foreground, silhouettes with lost detail | `shadows` positive |
| Flat, low-contrast | `contrast`, `blacks` negative, `clarity` |
| Haze in distant landscape | `dehaze` (10–40; more looks artificial) |
| Dull colour | `vibrance` first, `saturation` sparingly |
| One colour wrong (sky too cyan, foliage too yellow) | `hsl.<band>.hue / saturation / luminance`, bands: red orange yellow green aqua blue purple magenta |
| Mood | `grade.shadows` / `grade.highlights` (hue 0–360, saturation), `curve` |
| Horizon, framing | `rotate` (degrees), `crop.left/top/right/bottom` (0–1) |

Sliders run −100…100 with 0 meaning "no change", like other raw developers.

## Reading the stats

- `tones.p50` is the median lightness (0–100). Typical daylight scenes sit
  around 45–60; snow and fog higher; dusk lower. It is a description, not
  a target: a sunset silhouette is supposed to be dark.
- `clipped_highlights_pct` above ~1% on a scene without the sun or lamps in
  frame means lost highlight detail: lower `exposure`, `highlights` or
  `whites`. `sensor_clipped_pct` is what the camera already lost; no
  setting brings that back.
- `crushed_shadows_pct` above a few percent: raise `shadows` or `blacks`.
- `zones` is a 3×3 lightness grid (top row first): a quick check of sky
  versus foreground balance.
- `neutral_cast` far from `[0, 0]` suggests a colour cast in greys
  (a: green− / magenta+, b: blue− / yellow+). Snow in shade is legitimately
  a little blue; do not neutralise mood away.
- `hues_pct` shows which colour bands are present — adjust only those.

## Judgement

- Preserve the scene's character. Fog stays soft, dusk stays dark, snow is
  bright but keeps texture. Do not push every photo to the same brightness.
- Prefer several moderate settings to one extreme one. Values beyond ±60
  are rarely right; if you need them, a different parameter is probably
  the correct tool (e.g. `exposure` instead of `shadows=100`).
- White balance: trust the as-shot value unless the picture has an evident
  cast. Sunsets want their warmth; do not "correct" it.
- Lifting shadows reveals noise. After a strong lift check a dark region
  at 100% and raise `denoise` if needed.
- People: keep skin natural — go easy on `clarity`, `dehaze`, `saturation`.
- When unsure between two versions, choose the more natural one.

## Batch

For a folder, run the loop per photo. A quick pass is `rawsmith auto` on
each file, then review the previews and refine only those that need it.

## Limits

Sony ARW (uncompressed and lossy-compressed) from supported cameras only;
`inspect` reports an `unsupported` error otherwise. Adjustments are global;
there are no brushes or masks. Output is sRGB.
