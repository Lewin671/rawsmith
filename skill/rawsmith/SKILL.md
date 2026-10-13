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
preview, **looking at it**, and repeating. Successful subcommands print JSON
on stdout; errors print JSON on stderr with exit status 1 and empty stdout.
I/O errors name the path. `--help` and `--version` print plain text.
RAW, recipe and output paths must be valid UTF-8. Invalid paths return a
`params` JSON error before any file is written; the message names the path
with invalid bytes replaced by the Unicode replacement character (lossy text).
Outputs are replaced; RAWs never. Use a new output name to keep a version.
`output_replaced` reports replacement (`null` when no image was written).

## The loop

Check `rawsmith --version` before starting. If the binary is not on PATH,
try `~/.local/bin/rawsmith`, the release installer's default, and use that
absolute path for the commands below. If neither works, follow the
installation instructions linked from the rawsmith README before editing
photos.

1. **Inspect** — `rawsmith inspect photo.ARW`
   Camera settings, as-shot white balance, and how the sensor was exposed
   (`saturated_pct`: data that is gone for good; `headroom_stops`: unused
   highlight room). `compression` reports `uncompressed`,
   `lossy-compressed` or `lossless-compressed`. Lossless Bayer full-frame L
   and APS-C M are supported. Reduced-size YCbCr full-frame M/S and APS-C S
   return `unsupported`; changing recipes cannot make those modes decode.

2. **Start from auto** — `rawsmith auto photo.ARW -o preview.jpg --size 1400`
   Analyses the scene, saves a starting recipe next to the RAW
   (`photo.ARW.rawsmith.json`) and explains each decision. Auto proposes a
   measured starting point. Read its evidence and bounds;
   ambiguous scenes and local lighting still need visual editing.
   Its median floor allows positive exposure after highlight compression;
   compression yields if necessary, and later tone trials back off the
   control that breaches the floor or clipping bounds.
   Auto sets a zero base `defringe` to 50 when more than 0.5% of framed
   native tile samples border sensor loss (any photosite at 98%) and a
   strong edge (square-root green difference >= 0.25), a one-in-200
   coverage gate against isolated loss.
   Read the reported risk evidence; non-zero base defringe is always kept,
   and purple colour without sensor loss does not enable it.

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

5. **Compare in one image** —
   `rawsmith compare photo.ARW -o comparison.jpg`.
   Open it: the upright camera JPEG is on the left and the current saved
   edit on the right, with the edit's crop and rotation applied to the
   camera. No labels are drawn; `panels` lists them left to right with
   `kind`, `recipe_file`, final `width`/`height`, framing and colour space.
   To compare rounds, keep earlier recipes using separate `--recipe FILE`
   paths, then use
   `rawsmith compare photo.ARW --panel recipe:earlier.json --panel saved -o rounds.jpg`.
   Repeat `--panel` for `camera`, `saved`, `default` or `recipe:FILE` (at
   least one); a missing explicit recipe is an error. `saved` uses defaults
   if the default sidecar is absent; `default` always ignores saved edits.
   Recipes keep their own framing; the camera matches the first non-camera
   panel. `--panel camera` alone exports the upright camera rendering with
   neutral framing. Read `framing_matched` and `warnings` when framing differs.
   `--size` defaults to 1400 per panel; all panels shrink to one height.
   Orientations outside 1/3/6/8 are rejected. Camera colour uses embedded
   JPEG declarations first, then RAW EXIF. `color_declaration` reports
   `source` and the original `color_space`; the panel's `color_space` is
   its final encoding. Declared Adobe RGB (1998) is converted to sRGB
   (out-of-gamut colours are clipped), reported in `color_conversion`;
   declared or converted sRGB rows are tagged. Unknown, conflicting,
   uncalibrated or unverified ICC colour keeps unconverted camera pixels,
   a warning and `output_color_space: "mixed-unmanaged"` with no profile.
   Use established or converted sRGB references for colour judgement.
   `compare` never saves recipes and strips camera metadata.
   Keep the change only if the picture is better. If not, restore the
   earlier recipe or set the values back. Stop when another round would
   not visibly improve the picture — usually after 3 to 6 rounds. More
   rounds tend to over-process.

6. **Check detail at 100%** before delivering —
   `rawsmith render photo.ARW --region 0.4,0.4,0.6,0.6 -o detail.jpg`
   `--region` is `left,top,right,bottom` as fractions of the finished
   (upright, cropped) picture, rendered at full resolution. Look for noise
   in lifted shadows, harsh sharpening, halos and lost texture. Previews
   develop the same native-resolution pixels and area-resize the finished
   sRGB image; they take full-resolution processing work and memory.
   Resizing hides fine artefacts, so inspect 100% even when the preview is
   clean. Different viewer filters and JPEG compression can change the
   appearance of a resized export slightly.

7. **Export** — `rawsmith render photo.ARW -o final.jpg` (full resolution;
   `.png` gives 16-bit). Both declare sRGB: an ICC v2 profile in JPEG, the
   standard sRGB chunk in PNG. `output_color_space` reports `"sRGB"`
   (`null` without an image output). Read the full-resolution clipping
   numbers too. Report what you changed and why, in plain words.

`rawsmith schema` lists every parameter with range and meaning. Use
`rawsmith render ... --set k=v` to try something without saving it (the
recipe it prints includes the trial values, but the recipe file is not
touched; repeat the `--set` with `edit` to keep it), and
`rawsmith edit ... --reset` to start over.

The default is a finished rendering with photographic contrast and colour
hue preserved through tone mapping. Muted colour separation follows local
tone contrast, fading for vivid colours; there is no fixed saturation boost.
Auto replaces global tone and colour, measuring candidates against that
current base rather than stacking on saved tone/colour. It measures sensor loss, native
noise, relative colourfulness and spatial tone evidence. Unset/null denoise
uses the same native measurement as default rendering; an explicit amount
is kept. Weak evidence leaves
smaller adjustments, and clarity stays at zero. Its foreground bound can
leave bright sky above the highlight target because lost sensor information
cannot be recovered by darkening the subject. Check channel clipping as well
as pure white, and review the reported confidence before accepting the start.
Auto only applies global adjustments. It never changes white balance,
crops, or adds gradients, so a landscape with a bright sky usually still
needs a gradient. Its `decisions` report a cast when greys dominate a
confident high-key scene. Blue shade and warm lighting are left as shot.

Version 0.2.0 renders older sparse recipes with the new defaults; their
writer version does not pin pixels. Check `recipe_compatibility.note` in
render reports (or each developed compare panel). Trials and comparison do
not rewrite recipes. Use the original executable to reproduce an old look.

The recipe lives next to the RAW by default. To keep it elsewhere, or to
keep several versions of one photo, pass the same `--recipe FILE` to every
`auto`, `edit` and `render` call for that version.
`edit` and `auto` can create that file; `render` reports an error if an
explicit recipe file does not exist. Even without a preview, edits check
the RAW and white balance before saving.
`recipe_file` reports the file read by `render` (`null` when a missing
default sidecar means defaults), or the file saved by `edit`/`auto`.

Validation, decoding, rendering, encoding and staging failures preserve
existing recipes and outputs; no final image or recipe is created. An
image and recipe are staged before either is replaced, and ordinary commit
failures roll back the first replacement. The pair is not jointly atomic:
process termination, power loss, concurrent destination changes or a failed
rollback are not covered. If rollback fails, read the error's recovery
path for the retained old output backup, when one exists.

Rendering is deterministic: the same binary, RAW, recipe and render options
give a byte-identical file, so re-rendering a restored recipe reproduces the
earlier picture exactly.

Developed images and comparison rows contain no camera metadata.
`extract-jpeg` is a byte-for-byte copy of the embedded JPEG and may retain
metadata; do not assume it is
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
| Residual thin purple/blue colour on strong edges | additional `defringe` (0–100, default 0); try with `render --set`, inspect at 100%, then save only if genuine colours survive |
| Dark or bright corners | `vignette` (+ brightens corners) |
| Mood | `grade.shadows` / `grade.highlights` (hue 0–360, saturation), `curve` |
| Horizon, framing | `rotate` (degrees), `crop.left/top/right/bottom` |

Tone and colour sliders generally run −100…100 with 0 meaning "no change".
`dehaze`, `denoise`, `sharpen` and `defringe` run 0…100; use `schema` for
each control's exact range and default.

**Defringe** adds suppression of narrow, unsupported purple/blue colour at
luminance edges and residual highlight fringes. Neutral thin-edge protection
already runs automatically inside demosaicing, with no recipe control. Lens
corrections already correct lateral CA; they do not remove every kind of
fringe. `defringe` preserves luminance and supported purple surfaces, but can
affect truly purple detail only a few sensor pixels wide. This additional
filter is off by default; `defringe=0` leaves automatic demosaic protection on.
Check purple lighting, flowers and fine coloured detail before saving; do not
use global purple/magenta HSL desaturation as a substitute. It cannot restore
lost branch shape, clipped highlight texture, a broad purple cast or wide
out-of-focus halos. Previews resize the filtered native-resolution result
and can hide damage to fine colour; use `--region` to decide the final strength.

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

Statistics describe that render's size and region. Small clipped patches
can average away in a small preview, so clipping percentages can differ
slightly from the full-resolution export. Check detail and read the final
export report before deciding that highlight or shadow detail is safe.

- `tones.p50` is the median lightness (0–100). Typical daylight scenes sit
  around 45–60; snow and fog higher; dusk lower. It is a description, not
  a target: a sunset silhouette is supposed to be dark.
- `clipped_highlights_pct` counts pixels with all three encoded sRGB
  channels ≥0.998; `channel_clipped_pct` counts any channel, and
  `channel_clipped_pct_rgb` gives R/G/B separately. These counts use every
  output pixel, before file dithering. Above ~1% pure white without sun or
  lamps in frame calls for checking lost detail and trying lower `exposure`,
  `highlights` or `whites`. A zero pure-white count does **not** make a
  highlight safe: inspect bright regions at 100% for pink/magenta edges,
  coloured speckles and hard transitions. High channel counts can also be
  legitimate vivid colours at the output ceiling; adding saturation can
  flatten them. Use the image, not a count alone, to decide.
- `sensor_clipped_pct` is the percentage of full-active-area 2×2 CFA cells
  with any sensor-saturated photosite, at 98% of black-subtracted range,
  before hot-pixel repair or WB. Render identifies this as
  `sensor_clipped_scope="full_active_area"`; it is unchanged by crop,
  `--region`, output size or recipe and matches `inspect.exposure.sensor_clipped_pct`.
  `inspect.exposure.saturated_pct` gives each colour's photosite percentage;
  green has two sites per cell, so its denominator is different.
  `saturated_cells_pct` gives R/G/B cell percentages (either green site),
  which share the union's denominator. No edit restores sensor-saturated
  detail. If this count is nonzero, check the sun, lamps, bright sky or snow
  even when output clipping is zero. Prefer a natural transition to white
  over false colour; lowering output clipping alone is not a repair.
- `region` echoes the requested normalized `[x0,y0,x1,y1]` in the
  rotated/cropped frame, or null for the whole frame. Keep it with the detail
  report so the check can be reproduced. Output stats describe that region;
  the sensor clipping statistic still describes the full active area.
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
  `denoise` null) combine measured native noise with an ISO prior in both
  default renders and auto. Read `noise_reduction.strength` for the amount
  used and `source` for measured/prior/explicit provenance. Set an explicit
  number to adjust from it; `denoise+=5` cannot increment a null recipe.
  `denoise=0` disables it, and `denoise=null` restores measurement.
  `denoise` smooths chroma grain first, adds a broader chroma correction
  gradually above 25, and mild luminance smoothing above 50. High values
  retain fine luminance texture but cannot recover focus or motion blur;
  residual noise still needs a 100% check.
- For the 100% check choose the subject, the darkest area you lifted,
  and a patch of plain sky (dust shows there, especially after `dehaze`
  or a darkening gradient).
- Snow and fog: auto needs broad bright neutrals and a tonal range before
  lifting them; uniform grey water and atmospheric scenes are ambiguous. Bright snow sits around lightness 85–93 at `tones.p95`; use
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

Sony A7 IV (ILCE-7M4) only.

| Encoding | Supported modes |
|---|---|
| Uncompressed Bayer | Full frame and APS-C |
| Lossy-compressed Bayer | Full frame and APS-C |
| Lossless-compressed Bayer | Full-frame L and APS-C M |
| Lossless YCbCr | Unsupported: full-frame M/S and APS-C S |

An unsupported mode returns a JSON `unsupported` error before writing.
Local adjustments are limited to graduated filters: no brushes, subject masks, or spot and dust
removal. If you see sensor dust, a stray object or a hot pixel, tell the
user; cropping is the only workaround. Output is sRGB.
