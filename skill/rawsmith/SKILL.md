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
  for mood, a tone curve for the contrast you want, gradients, local masks and
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

2. **See the starting points** — if the photo already has a recipe
   (`photo.ARW.rawsmith.json`, or one the user points to), it holds an
   earlier edit: render it and continue from there, or give a fresh start
   its own `--recipe` file. `auto` replaces the tone, colour and local
   settings of the recipe it writes to.
   `rawsmith auto photo.ARW -o start.jpg --size 1400 --brief` saves a measured
   starting recipe and explains each choice in `decisions`;
   `rawsmith compare photo.ARW -o cmp.jpg --size 1400` shows the camera's JPEG (left)
   beside the saved recipe (right), upright and in the same framing
   (add the same `--recipe FILE` when the recipe lives elsewhere).
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
   `rawsmith edit photo.ARW --set exposure=0.3 --set shadows=25 -o preview.jpg --size 1400 --brief`
   — one `--set` per assignment, repeated (`key=value` sets, `key+=n` /
   `key-=n` nudges); the recipe is saved and a preview rendered. Quote
   anything with brackets or braces (`--set 'curve=[[0.25,0.2],[0.75,0.8]]'`)
   so the shell does not expand it. Open every preview.
   - *Composition*: `crop`, `rotate`.
   - *Light*: `exposure`, `contrast`, `highlights`, `shadows`, `whites`,
     `blacks`, `curve`; `gradients` or `masks` to shape local light;
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
   Read `losses_against_camera` in the report for your panel: it lists
   what the edit gave up against the camera's rendering — a lifted black
   point, flatter tones, new pure white or black, a colour gone weak and
   pale. An edit that opens shadows and brightens a subject usually pays
   for it there. For each line, either fix it (`blacks`, `contrast` or a
   `curve` for a black anchor; a gradient or luma mask to hold the sky;
   less `hsl` desaturation) or be able to say why this picture wants it.
   An empty list is not a verdict; it only means these particular losses
   were not measured.

6. **Check at 100%** —
   `rawsmith render photo.ARW --region 0.4,0.4,0.6,0.6 -o detail.jpg --brief`
   (`left,top,right,bottom` as fractions of the finished, cropped
   picture; always full resolution, so it cannot be combined with
   `--size` — use a smaller region instead). Look at the subject, the darkest area you lifted, a patch of
   smooth sky, and any hard edge against a bright background. Strong
   settings are where artefacts come from: noise in lifted shadows,
   blotches from `dehaze`/`clarity` in smooth areas, jagged edges from
   extreme `hsl` luminance, halos, coloured fringes. Back off the setting
   that caused it rather than accepting the artefact.

7. **Preserve a baseline and try a focused improvement.** Save your best
   finished recipe as `baseline.json`, then copy it to `candidate.json`.
   Try up to two useful changes to a concrete remaining weakness: framing,
   subject emphasis, local light or noise/detail. Reposition masks after
   changing crop. Keep the baseline when no worthwhile improvement exists.
   Do not replace it merely because a candidate is newer.
   Use distinct per-photo filenames for batches.

   When Codex viewing these previews is already authorized, use the installed
   `rawsmith-select` (or `~/.local/bin/rawsmith-select`):

   ```sh
   rawsmith-select photo.ARW --baseline baseline.json --candidate candidate.json \
     --recipe selected.json --output final.jpg --allow-cloud
   ```

   Use new output names: the helper refuses existing destinations and leaves
   all inputs alone. It compares both orders, then requires fresh acceptance
   and control checks before adopting a candidate. Ties, failures and
   disagreement keep the baseline. Read its JSON report; fallback is not a
   successful improvement. It requires Python 3.8+ and Codex and sends previews
   to Codex. Do not pass `--allow-cloud` without authorization. If authorization
   or the helper is unavailable, compare visually in this session —
   `rawsmith compare photo.ARW --panel camera --panel recipe:baseline.json --panel recipe:candidate.json -o pick.jpg`
   at the default size (smaller panels hide the difference) — and keep
   the baseline on doubt. Check selected native detail before delivery and
   restore the baseline if it introduces a defect. Deliver only one image.

8. **Export** — the helper already exported its selected native JPEG. If
   you selected visually, render the chosen recipe with
   `rawsmith render photo.ARW --recipe chosen.json -o final.jpg --brief` (full
   resolution, sRGB). Default JPEG quality 92 is right for almost everything.
   For “smaller file”, “for the web” or “under N MB”, use `--max-bytes` (decimal MB)
   or `--quality 85` with a `--size`; choose the settings without asking for a number.
   For maximum fidelity or further editing, export `.png` (16-bit); tell the user the final file size from `output_bytes`.
   Open the exported file. Tell
   the user in two or three plain sentences what you made of the picture
   and why — the intent, not a list of sliders.

**`render --set` is a trial: it never saves the recipe. `edit` saves the
recipe for the whole picture, even with `--region`; only its output image
is restricted to the region.** Check `recipe_saved` and the saved-region
`note` in brief reports. `edit --reset` starts over; `rawsmith schema` lists every
parameter with its range and meaning. By default the recipe is saved
next to the RAW as `photo.ARW.rawsmith.json`; pass the same
`--recipe FILE` to `auto`, `edit`, `render` and `compare` to keep it
elsewhere or to keep several versions.

## The controls

| To do this | Use |
|---|---|
| Strengthen the composition | `crop.left/top/right/bottom` are edge positions 0–1 measured from the left/top: `crop.top=0.1` removes the top tenth, `crop.bottom=0.9` the bottom tenth, `crop.right=0.85` the right 15%; `rotate` |
| Overall brightness | `exposure` (stops) |
| Warmth, colour of the light | `temperature` (K, higher is warmer), `tint` (+ magenta, − green) |
| Tame a bright sky; open dark areas | `highlights` negative; `shadows` positive |
| Set the white and black points | `whites`, `blacks` |
| Overall and local contrast | `contrast`, `clarity`, `curve` |
| Cut through haze | `dehaze` |
| More or less colour overall | `vibrance` (protects vivid colours), `saturation` |
| Build a palette | `hsl.<band>.hue / saturation / luminance` — bands: red orange yellow green aqua blue purple magenta |
| Mood | `grade.shadows` / `grade.highlights` (`hue` 0–360, `saturation`), `grade.balance` |
| Local subject, corner or sky through branches | `masks` (radial, linear, luminance and colour selection); `gradients` for a simple fade |
| Hold the eye in the frame | `vignette` (negative darkens the corners) |
| Purple or pink edges and coloured specks on branches against bright sky | `defringe` |
| Noise; crispness | `denoise`, `sharpen` |

Tone and colour controls run −100…100 with 0 meaning no change; `schema`
has exact ranges.

**Palette with HSL.** Hue shifts turn one colour toward its neighbour
in the order red → orange → yellow → green → aqua → blue → purple →
magenta → red: positive moves toward the next band, negative toward the
previous (`hsl.green.hue=15` deep teal, `hsl.blue.hue=-15` cyan,
`hsl.yellow.hue=-15` gold). Which band a colour falls in is listed in
the full (non-brief) report's `hues_pct` and compare's `look.colours`;
luminance makes a colour heavier or lighter (darker blue for a deeper
sky, lighter yellow so leaves glow); saturation quietens colours that
compete with the subject. This is how a picture gets a considered
palette rather than "more colour". Keep luminance within about ±55:
beyond that fine edges turn jagged. HSL acts in proportion to how
colourful a pixel is, so muted colour — leaves in shade, olive foliage,
a dull roof — barely moves; to brighten those, use a `colors` mask with
`exposure` (see local masks).

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

**Local masks.** Use a radial to lift a pavilion or face, or darken a blurred
foreground corner without a straight gradient reaching a nearby tree. Use a
luma range for bright sky behind dark branches or roofs; multiply it by a
radial/linear shape when only part of that brightness range needs editing.
Use `colors` to select by hue — the HSL band names — when the subject has
a colour its surroundings lack: gold leaves against blue sky, a blue car
on brown leaves, a red roof among trees. It is the nearest thing to a
subject mask: lift or saturate the leaves without lighting the sky
between them.

```sh
rawsmith render photo.ARW --set 'masks=[{"shape":"radial","center":[0.6,0.5],"radii":[0.18,0.22],"feather":0.6,"exposure":0.7}]' --show-mask 0 -o mask.png --size 1400
rawsmith edit photo.ARW --set 'masks+={"luma":[0.75,1],"luma_feather":0.08,"exposure":-0.4}' -o preview.jpg --size 1400
rawsmith edit photo.ARW --set 'masks+={"colors":["yellow","orange"],"exposure":0.5,"saturation":10}' -o preview.jpg --size 1400
```

Check placement first with `render --show-mask N`: white is selected, black
excluded, N is a zero-based **masks** index. Trial assignments are not saved;
apply the same assignment with `edit` after checking. Check the photo and mask
with a full-resolution `--region` across the boundary, not just a preview.
Radial centre and radii are fractions of finished width/height; `rotation` is
clockwise, `feather` fades inside the ellipse, and `invert:true` selects outside.
Default shape is linear with `from`/`to`; omit geometry for luma or colors alone.
Luma uses display lightness 0–1 before the new local edits and finishing controls;
its guided refinement suppresses speckle and preserves thin high-contrast edges.
It is a brightness selection, not recognition of a sky or subject.
`colors` selects pixels whose hue lies in the named bands (neighbouring bands
blend smoothly); greys and near-neutral pixels are left out, and its edges
follow lightness. Shape, luma and colors multiply. Anything else of the same
colour is selected too — crop it out, or limit the selection with a radial.

`masks=[...]` replaces up to eight entries; `masks+={...}` appends one;
`masks-=INDEX` removes one in current-list order. Available adjustments are
exposure (−3–3 stops), saturation, contrast, shadows and highlights (−100–100).
Contrast is an offset to global filmic contrast. Local temperature/tint is
not available. Change framing first, then reposition masks.
Strong lifting still exposes noise; darkening sensor-lost sky still makes grey.
For a blurred foreground corner, start with a radial alone: a brightness
threshold can reshape the soft edge.
A radial lift over a smooth surface — a hazy hillside, sky, water, a plain
wall — shows as a pale glow in the shape of the ellipse. Look for that shape
in the preview. Keep such a lift small (about 0.2 stops), or restrict it to
the subject's own brightness with `luma` so the surroundings stay put.

**White balance** starts as shot. To nudge it, set a number first (the
as-shot values are in rendered reports' `white_balance`);
`--set temperature=null` returns to as shot.

**Speed.** Renders of one photo reuse its developed frame, so changing
tone, colour, crop, masks or the output size is quick. Changing
`temperature`, `tint`, `denoise`, `defringe` or `lens_corrections`
develops the photo again and takes a few seconds longer: settle white
balance and noise early, then iterate on everything else.

**Noise and detail.** With `denoise` unset the amount is measured from
the file (`noise_reduction.strength` in the report); set a number to
change it. `dehaze` and `clarity` amplify noise in smooth areas at high
ISO — if the sky turns blotchy, lower them first. The measured amount
already clears grain from smooth areas while keeping edges and firm
texture. Raise it by 10–20 only if a 100% patch of lifted shadow or
night sky still shows grain; every step up also smooths fine texture
(foliage, fabric, distant detail), so look at that too.

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
  and positions for gradients, masks and regions depend on the crop.
- Light before colour: decide what is bright and what recedes first.
- Lifting is not free: every stop of `shadows` and `exposure` takes depth
  away. After opening a picture up, give it back a real dark (`blacks`,
  `contrast`, an S-`curve`) so it does not turn grey and flat.
- Subjects in shade want lifting; surroundings that compete want
  darkening. Use a radial for the subject or a corner, and luma to protect dark occluders.
- A scene with one dominant colour (autumn leaves, a blue hour, a sunset)
  gets stronger when the other colours are quietened, not when everything
  is saturated.
- Warmer white balance and a warm grade together turn a mixed palette
  into one brown: a blue subject goes grey, green moss and red leaves
  vanish. Use one of them, and when `compare` says a colour "has nearly
  gone", bring it back (`hsl.<band>.saturation`, a `colors` mask) unless
  removing it was the point.
- Keep the scene's character: fog stays soft, dusk stays dark, snow is
  bright and keeps its texture, a sunset keeps its warmth.
- Snow and fog: bright snow sits around 85–93 at `tones.p95`; use
  `exposure` and `whites`, and hold a clear sky with a gradient.
- `highlights` far negative on a scene that is bright everywhere greys
  the whole picture; give exposure back or use a gradient.
- With the sun or a blown sky in frame, keep `highlights` above about
  −40: further down the lost area turns into a grey slab or a disc with
  a hard edge, and fine branches against it pick up coloured specks.
- A deep blue night or dusk sky often already sits at the edge of what
  sRGB can show, so `saturation`, `vibrance` and `hsl.blue.saturation`
  barely move it. Turn it towards purple first (`hsl.blue.hue` +10…+20)
  and lift it a little (`hsl.blue.luminance` +10…+20); then it can take
  more colour. compare's "blue has turned towards aqua" note points here.
- `dehaze` also strengthens colour: on a blue haze it quickly turns the
  whole frame a saturated blue. Pair it with less `vibrance` or
  `hsl.blue.saturation`, and raise `denoise` a little if the haze turns
  grainy.
- `blacks` moves the black point by at most 8% of the display range, so
  on a hazy or lifted picture whose darkest tones sit far above black it
  barely changes anything; anchor the dark with a curve point
  (`[0.3,0.2]`) or `dehaze` instead.
- Snow, frost and white cloud: check them at 100% after raising `whites`
  or `exposure`; texture goes before the preview shows it. Hold them with
  `highlights` instead.
- A top `gradient` stacked with a strong `vignette` turns sky corners
  navy or black; use one, or back both off.
- `colors` masks select by hue, so `yellow` also takes yellow-green
  foliage and `blue` takes blue water. Muted colour is only partly
  selected — it fades in with chroma — so shaded or olive leaves respond
  less than the vivid ones.
- Purple or pink edges and coloured specks on twigs against a blown sky:
  `defringe` 50–90, up to 100 for twigs around the sun; look at a 100%
  `--region` of the twigs, since previews hide it (check purple flowers,
  thin red details and small coloured lights at 100% first). It does not
  fix wide out-of-focus halos.
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

Local adjustments support four legacy gradients plus eight linear/radial masks
with optional luminance and colour selection. No brushes, semantic subject
masks, local white balance, spot/dust removal or black-and-white mixer.
If you see sensor dust, a stray object or a hot pixel, tell the user;
cropping is the only workaround. Output is sRGB JPEG or 16-bit PNG
without camera metadata.
