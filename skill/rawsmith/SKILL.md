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

You are the photo editor. `rawsmith` turns a RAW file plus a small JSON
*recipe* into a picture; the RAW is never modified. You change recipe
parameters, render, **look at the result**, and repeat until the picture
is finished.

## What "finished" means

The user's camera already made a competent JPEG of this scene. Your job
is to deliver a picture that is **clearly better than that JPEG at a
glance** — not the same picture with tidier numbers.

- **Decide; do not ask.** People can rarely describe the look they want.
  Unless the user gave a direction, choose the interpretation yourself and
  deliver one finished picture. Never ask which style they prefer and
  never hand back a set of options to choose from. If they did say what
  they want, that comes first.
- **Have an intent before touching a control.** Look at the photo and say
  in one sentence what it is about and how it should feel: *"the lit
  pavilion in a dark, quiet garden"*, *"cold blue dusk with warm street
  lights"*, *"gold leaves glowing against evening shade"*. Every setting
  then serves that sentence. If you cannot say it, you are only correcting.
- **Commit to it.** A timid edit that lifts shadows a little and adds a
  touch of colour looks like the camera's JPEG and is not worth making.
  Use the whole kit: crop for composition, white balance as a creative
  choice, per-colour hue and luminance to build a palette, colour grading
  for mood, a tone curve for the contrast you want, gradients and
  vignette to lead the eye.
- **Stay believable.** The result must still read as a photograph of that
  place and light: no garish colour, no halos, no crushed or blown mess,
  no gimmick. Do not convert to black and white or swap the scene's
  colours for a different palette unless asked. Keep skin natural.
- **Judge against the camera.** Before delivering, put your edit beside
  the camera's JPEG (`rawsmith compare`). If a stranger could not tell
  within a second which one was edited, or would not prefer yours, you
  are not done: strengthen the idea or rethink it.
- **When torn, make the versions yourself.** Save two or three
  interpretations as separate recipes, compare them side by side, pick
  the best and deliver only that one.

## The loop

Use `~/.local/bin/rawsmith` if `rawsmith` is not on PATH. Successful
commands print JSON on stdout; errors are JSON on stderr with exit
status 1. Output files are replaced, RAWs never; use a new output name to
keep a version.

1. **Inspect** — `rawsmith inspect photo.ARW`
   Camera settings, white balance, and how the sensor was exposed
   (`saturated_pct` is data gone for good; no edit brings it back).

2. **See the starting points** —
   `rawsmith auto photo.ARW -o start.jpg --size 1400 --brief` saves a measured
   starting recipe and explains each choice in `decisions`;
   `rawsmith compare photo.ARW -o cmp.jpg --size 1400` shows the camera's JPEG (left)
   beside the saved recipe (right), upright and in the same framing.
   Open both images. `auto` aims for a sound, finished-looking base —
   brightness, contrast, colour and noise reduction from what it
   measured — but it does not interpret, and it never crops, changes
   white balance or adds local adjustments. Build on it; the camera's
   JPEG is the bar to beat.

3. **Decide the picture.** State the intent sentence. Then list what
   stands between the current render and that picture: framing first
   (what should be cropped away), then light (where the eye should go,
   what should recede), then colour (the two or three colours that carry
   the picture, and what should be quiet).

4. **Build it in passes** with
   `rawsmith edit photo.ARW --set key=value ... -o preview.jpg --size 1400 --brief`
   (`key=value` sets, `key+=n` / `key-=n` nudges; the recipe is saved and
   a preview rendered). Open every preview.
   - *Composition*: `crop`, `rotate`.
   - *Light*: `exposure`, `contrast`, `highlights`, `shadows`, `whites`,
     `blacks`, `curve`; `gradients` to darken a sky or lift a foreground;
     `vignette` to hold the eye in the frame.
   - *Colour*: `temperature` / `tint`, `vibrance`, `hsl.<band>.*`,
     `grade.shadows` / `grade.highlights`.
   - *Finish*: `clarity`, `dehaze`, `sharpen`, `denoise`, `defringe`.
   Make changes large enough to see. If a preview looks the same as the
   last one, the change was too small to matter.

5. **Compare** — `rawsmith compare photo.ARW -o cmp.jpg --size 1400` after each real
   step, and against earlier versions kept as separate recipes:
   `rawsmith compare photo.ARW --panel camera --panel recipe:a.json --panel recipe:b.json -o versions.jpg`.
   Panels are unlabelled; the report's `panels` lists them left to right.
   `--size` bounds each panel's longest edge (default 1400), not the whole row.
   Keep a change only if the picture is better. Apply the test in
   "What finished means" here.

6. **Check at 100%** —
   `rawsmith render photo.ARW --region 0.4,0.4,0.6,0.6 -o detail.jpg --brief`
   (`left,top,right,bottom` as fractions of the finished, cropped
   picture). Look at the subject, the darkest area you lifted, a patch of
   smooth sky, and any hard edge against a bright background. Strong
   settings are where artefacts come from: noise in lifted shadows,
   blotches from `dehaze`/`clarity` in smooth areas, jagged edges from
   extreme `hsl` luminance, halos, coloured fringes. Back off the setting
   that caused it rather than accepting the artefact.

7. **Export** — `rawsmith render photo.ARW -o final.jpg --brief` (full
   resolution, sRGB; `.png` gives 16-bit). Open the exported file. Tell
   the user in two or three plain sentences what you made of the picture
   and why — the intent, not a list of sliders.

**`render --set` is a trial: it never saves the recipe. `edit` saves the
recipe for the whole picture, even with `--region`; only its output image
is restricted to the region.** Check `recipe_saved` and the saved-region
`note` in brief reports. `edit --reset` starts over; `rawsmith schema` lists every
parameter with its range and meaning. By default the recipe is saved
next to the RAW as `photo.ARW.rawsmith.json`; pass the same
`--recipe FILE` to `auto`, `edit` and `render` to keep it elsewhere or to
keep several versions.

## The controls

| To do this | Use |
|---|---|
| Strengthen the composition | `crop.left/top/right/bottom` (edge positions 0–1: `crop.top=0.1` removes the top tenth), `rotate` |
| Overall brightness | `exposure` (stops) |
| Warmth, colour of the light | `temperature` (K, higher is warmer), `tint` (+ magenta, − green) |
| Tame a bright sky; open dark areas | `highlights` negative; `shadows` positive |
| Set the white and black points | `whites`, `blacks` |
| Overall and local contrast | `contrast`, `clarity`, `curve` |
| Cut through haze | `dehaze` |
| More or less colour overall | `vibrance` (protects vivid colours), `saturation` |
| Build a palette | `hsl.<band>.hue / saturation / luminance` — bands: red orange yellow green aqua blue purple magenta |
| Mood | `grade.shadows` / `grade.highlights` (`hue` 0–360, `saturation`), `grade.balance` |
| Only the sky, or only the foreground | `gradients` |
| Hold the eye in the frame | `vignette` (negative darkens the corners) |
| Purple edges on branches against bright sky | `defringe` |
| Noise; crispness | `denoise`, `sharpen` |

Tone and colour controls run −100…100 with 0 meaning no change; `schema`
has exact ranges.

**Palette with HSL.** Hue shifts turn one colour toward its neighbour
(green toward deep teal, blue toward cyan, yellow toward gold);
luminance makes a colour heavier or lighter (darker blue for a deeper
sky, lighter yellow so leaves glow); saturation quietens colours that
compete with the subject. This is how a picture gets a considered
palette rather than "more colour". Keep luminance within about ±55:
beyond that fine edges turn jagged.

**Mood with grading.** `grade.shadows` and `grade.highlights` tint the
dark and bright parts separately — cool shadows with warm highlights is
the classic pairing; saturation 15–40 is visible without looking
filtered. Grading has almost no effect once `saturation` is near −100.

**Curve** points are `[input, output]` on display brightness, joined
smoothly, with `[0,0]` and `[1,1]` implied. An S-shape adds contrast:
`--set 'curve=[[0.25,0.2],[0.75,0.8]]'`. Lifting the first point off zero
(`[0,0.04]`) gives a soft matte black.

**Gradients** are graduated filters, full effect at `from`, fading to
none at `to`, positions as fractions of the finished picture:

```sh
# darken and enrich the sky from the top edge down to mid-frame, and lift the foreground
rawsmith edit photo.ARW --set 'gradients=[{"from":[0.5,0],"to":[0.5,0.5],"exposure":-0.6,"saturation":15},{"from":[0.5,1],"to":[0.5,0.6],"exposure":0.5}]'
```

`gradients=[...]` replaces the list (up to four); `gradients+={...}` appends
one filter; `gradients-=1` removes the second (zero-based current-list index).
Assignments run in order; a fifth append or invalid index fails without saving.
Place them again after changing `crop`. A
gradient also affects trees or buildings that cross it.

**White balance** starts as shot. To nudge it, set a number first (the
as-shot values are in rendered reports' `white_balance`);
`--set temperature=null` returns to as shot.

**Noise and detail.** With `denoise` unset the amount is measured from
the file (`noise_reduction.strength` in the report); set a number to
change it. `dehaze` and `clarity` amplify noise in smooth areas at high
ISO — if the sky turns blotchy, lower them first. Visible grain in a
smooth night sky usually needs `denoise` around 75–85; check a patch at
100% after changing it.

## Reading the report

Use `--brief` in the loop: output path/bytes, dimensions, tone percentiles,
clipping, cast, zones, denoise strength and warnings, plus short auto decisions.
Omit it for full decisions, settings, histogram and hue/chroma statistics.
Numbers support what you see; they do not replace looking.

- `stats.tones.p50` is median lightness (0–100): a description, not a
  target. A dusk scene is supposed to be dark.
- `clipped_highlights_pct` (all channels at white) and
  `channel_clipped_pct` (any channel): above about 1% without a sun or
  lamps in frame, look at the bright areas. Zero does not prove a
  highlight is clean — look for pink edges and hard transitions.
- `sensor_clipped_pct`: sensor-saturated data. Nothing restores it. A
  blown sky pulled down hard turns into a flat grey slab; crop it,
  or let it be white.
- `crushed_shadows_pct`: deliberate black is fine; several percent by
  accident is not.
- `zones` is a 3×3 lightness grid, top row first: sky against foreground.
- `neutral_cast` away from `[0, 0]` means greys carry a cast.
- `hues_pct` shows which colour bands are actually present — work on those.

Statistics describe that render's size and region; read the
full-resolution export's report as well.

## Craft notes

- Crop early. An empty sky or a distracting edge weakens everything else,
  and positions for gradients and regions depend on the crop.
- Light before colour: decide what is bright and what recedes first.
- Subjects in shade want lifting; surroundings that compete want
  darkening. With no brush, use gradients from the edges and a vignette.
- A scene with one dominant colour (autumn leaves, a blue hour, a sunset)
  gets stronger when the other colours are quietened, not when everything
  is saturated.
- Keep the scene's character: fog stays soft, dusk stays dark, snow is
  bright and keeps its texture, a sunset keeps its warmth.
- Snow and fog: bright snow sits around 85–93 at `tones.p95`; use
  `exposure` and `whites`, and hold a clear sky with a gradient.
- `highlights` far negative on a scene that is bright everywhere greys
  the whole picture; give exposure back or use a gradient.
- With the sun or a blown sky in frame, keep `highlights` above about
  −40: further down the lost area turns into a grey slab or a disc with
  a hard edge, and fine branches against it pick up coloured specks.
- Purple edges against a blown sky: `defringe` (check purple flowers and
  lights at 100% first). It does not fix wide out-of-focus halos.
- If two rounds in a row did not visibly improve the picture, the idea
  is exhausted: finish, or change the idea.

## Batch

For a folder, edit each photo with the full loop. For a quick pass, run
`rawsmith auto` on each file, review the previews, and give the full
treatment to the ones worth it.

## Limits

Sony A7 IV (ILCE-7M4) ARW only: uncompressed and lossy-compressed (full
frame and APS-C), and lossless-compressed in full-frame L and APS-C M.
Lossless M/S sizes are reported as unsupported.

Local adjustments are limited to graduated filters: no brushes, subject
or luminance masks, no spot or dust removal, no black-and-white mixer.
If you see sensor dust, a stray object or a hot pixel, tell the user;
cropping is the only workaround. Output is sRGB JPEG or 16-bit PNG
without camera metadata.
