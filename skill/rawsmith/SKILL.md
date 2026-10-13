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
   `--region` is `left,top,right,bottom` as fractions of the finished
   (upright, cropped) picture, rendered at full resolution, so it takes a
   few seconds. Look for noise in lifted shadows, harsh sharpening, halos
   on edges. A 100% crop of a dark area looks darker than it did in the
   small preview; that is perception, not a different rendering.

7. **Export** — `rawsmith render photo.ARW -o final.jpg` (full resolution;
   `.png` gives 16-bit). Report what you changed and why, in plain words.

`rawsmith schema` lists every parameter with range and meaning. Use
`rawsmith render ... --set k=v` to try something without saving it (the
recipe it prints includes the trial values, but the recipe file is not
touched; repeat the `--set` with `edit` to keep it), and
`rawsmith edit ... --reset` to start over.

Auto only sets global tone and colour. It never changes white balance,
crops, or adds gradients, so a landscape with a bright sky usually still
needs a gradient, and its `decisions` mention a colour cast when it sees
one in the greys.

The recipe lives next to the RAW by default. To keep it elsewhere, or to
keep several versions of one photo, pass the same `--recipe FILE` to every
`auto`, `edit` and `render` call for that version.
`edit` and `auto` can create that file; `render` reports an error if an
explicit recipe file does not exist. Even without a preview, edits check
the RAW and white balance before saving.

Developed images contain no camera metadata. `extract-jpeg` is a byte-for-byte
copy of the embedded JPEG and may retain metadata; do not assume it is
safe to share without checking it.

## Parameters that matter most

| Goal | Parameters |
|---|---|
| Overall brightness | `exposure` (stops) |
| Colour cast, warmth | `temperature` (K, higher = warmer), `tint` (+ magenta, − green) |
| Bright sky or snow too harsh | `highlights` negative |
| Dark foreground, silhouettes with lost detail | `shadows` positive |
| Dull greyish snow, fog or cloud | `whites` positive (stretches bright tones up to white), `exposure` |
| Flat, low-contrast | `contrast`, `blacks` negative, `clarity` |
| Haze in distant landscape | `dehaze` (10–40; more looks artificial and reveals sensor dust) |
| Dull colour | `vibrance` first, `saturation` sparingly; nearly colourless scenes (snow, fog) only respond to `saturation` |
| One colour wrong (sky too cyan, foliage too yellow) | `hsl.<band>.hue / saturation / luminance`, bands: red orange yellow green aqua blue purple magenta |
| Only the sky, or only the foreground | `gradients` (see below) |
| Dark or bright corners | `vignette` (+ brightens corners) |
| Mood | `grade.shadows` / `grade.highlights` (hue 0–360, saturation), `curve` |
| Horizon, framing | `rotate` (degrees), `crop.left/top/right/bottom` |

Sliders run −100…100 with 0 meaning "no change", like other raw developers.

**Crop** values are edge positions, not amounts: `crop.top=0.1` removes
the top tenth, `crop.bottom=0.9` removes the bottom tenth. Cropping
changes the aspect ratio unless you move opposite edges to match;
`--size` is always the long edge. After cropping, check that nothing
important is cut at the new edges.

**Gradients** are graduated filters for what global sliders cannot
separate — a sky that should be darker and richer, a foreground that
should be brighter. Full effect at `from`, fading to none at `to`:

```sh
# darken and enrich the sky: full effect at the top edge, gone by mid-frame
rawsmith edit photo.ARW --set 'gradients=[{"from":[0.5,0],"to":[0.5,0.5],"exposure":-0.6,"saturation":15}]'
# brighten the foreground from the bottom up, in addition
rawsmith edit photo.ARW --set 'gradients=[{"from":[0.5,0],"to":[0.5,0.5],"exposure":-0.6},{"from":[0.5,1],"to":[0.5,0.6],"exposure":0.5}]'
```

Setting `gradients` replaces the whole list. Place `to` where the effect
should end (usually the horizon). Positions are fractions of the finished
(cropped) picture: after changing `crop`, place the gradients again.

**Curve** points are `[input, output]` on display brightness, joined
smoothly; `[0,0]` and `[1,1]` are implied. A point moves its whole
neighbourhood, so pin what should stay: `--set 'curve=[[0.15,0.2],[0.4,0.4]]'`
lifts only the shadows. Prefer `shadows` / `highlights`; use a curve for
what they cannot do.

`+=` and `-=` need a parameter that has a number; `temperature` and
`tint` start as "as shot", so set them to a value first (the as-shot
values are in every render's `white_balance`). Setting only one of the two
keeps the other as shot; `--set temperature=null` returns to as shot.

## Reading the stats

- `tones.p50` is the median lightness (0–100). Typical daylight scenes sit
  around 45–60; snow and fog higher; dusk lower. It is a description, not
  a target: a sunset silhouette is supposed to be dark.
- `clipped_highlights_pct` is the share of the picture blown to pure
  white. Above ~1% on a scene without the sun or lamps in frame means
  lost highlight detail: lower `exposure`, `highlights` or `whites`.
  `sensor_clipped_pct` is what the camera already lost; no setting brings
  that back. `channel_clipped_pct` also counts vivid colours at the limit
  of the output (a saturated sunset sky); high values there mean colour is
  at its ceiling, so adding saturation will only flatten it.
- `tones.spread` is global contrast (below ~15 is flat); `chroma_mean` is
  colourfulness averaged over the frame (snow and fog about 1, hazy
  landscapes 2–3, colourful scenes 4–8, vivid above 10).
- `crushed_shadows_pct` above a few percent: raise `shadows` or `blacks`.
- `zones` is a 3×3 lightness grid (top row first): a quick check of sky
  versus foreground balance.
- `neutral_cast` away from `[0, 0]` suggests a colour cast in greys
  (a: green− / magenta+, b: blue− / yellow+); beyond about ±1 is visible.
  Snow in shade is legitimately a little blue; do not neutralise mood
  away. It reads `[0, 0]` when the picture has no near-grey areas at all,
  so a frame that is uniformly blue or orange shows up in `hues_pct`
  (one band above ~85%) rather than here. To find a neutral setting, try
  values with `render --set tint=...` and watch `neutral_cast`.
- `hues_pct` shows which colour bands are present among the clearly
  coloured pixels (greys are not counted) — adjust only those. In snow
  scenes sky, haze and shaded snow all count as blue, so `hsl.blue`
  moves nearly everything.
- `histogram` is the share of the picture in each tenth of the lightness
  range, darkest first.

## Judgement

- Preserve the scene's character. Fog stays soft, dusk stays dark, snow is
  bright but keeps texture. Do not push every photo to the same brightness.
- `highlights` is for areas that really are too bright. Where everything
  is bright (snow, fog) or nothing is, a strong negative value greys or
  flattens the whole picture; stay within about −20, or give some
  `exposure` back.
- `whites` brightens a clear sky as much as snow; hold the sky with a
  gradient if it goes pale.
- `blacks` and `contrast` are gentle on hazy, low-contrast scenes; use
  `dehaze` to set a firm black there.
- Prefer several moderate settings to one extreme one. Values beyond ±60
  are rarely right; if you need them, a different parameter is probably
  the correct tool (e.g. `exposure` instead of `shadows=100`).
- White balance: trust the as-shot value unless the picture has an evident
  cast. Sunsets want their warmth; do not "correct" it.
- Lifting shadows reveals noise. After a strong lift check a dark region
  at 100% and raise `denoise` if needed. The defaults (`sharpen` 40,
  `denoise` 10; auto raises `denoise` with ISO) suit most files.
- For the 100% check choose the subject, the darkest area you lifted,
  and a patch of plain sky (dust shows there, especially after `dehaze`
  or a darkening gradient).
- Snow and fog: auto aims high already, but grey snow is the classic
  failure. Bright snow sits around lightness 85–93 at `tones.p95`; use
  `exposure` and `whites` to get there while texture remains.
- `dehaze` also darkens deep shadows a little and deepens the colour of
  the haze itself; pair it with `shadows` if the foreground sinks.
- Warming the white balance mutes blue skies; a little
  `hsl.blue.saturation` brings them back.
- People: keep skin natural — go easy on `clarity`, `dehaze`, `saturation`.
- When unsure between two versions, choose the more natural one.

## Batch

For a folder, run the loop per photo. A quick pass is `rawsmith auto` on
each file, then review the previews and refine only those that need it.

## Limits

Sony ARW (uncompressed and lossy-compressed) from supported cameras only;
`inspect` reports an `unsupported` error otherwise. Local adjustments are
limited to graduated filters: no brushes, subject masks, or spot and dust
removal. If you see sensor dust, a stray object or a hot pixel, tell the
user; cropping is the only workaround. Output is sRGB.
