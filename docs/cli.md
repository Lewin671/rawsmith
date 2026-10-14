# CLI reference

Successful subcommands print JSON on stdout. Errors print JSON on stderr
with exit status 1 and nothing on stdout. The error object contains
`error.kind` and `error.message`; I/O errors name the relevant path.
RAW, recipe and output paths must be valid UTF-8. Invalid paths return a
`params` JSON error before any file is written; the message names the path
with invalid bytes replaced by the Unicode replacement character (lossy text).
`--help` and `--version` print plain text.

RAW files are only read. Validation, decoding, rendering, encoding and
staging failures leave existing recipes and outputs unchanged, and create
no final output or recipe. A single output is staged beside its destination
and renamed into place. `edit`/`auto` with an output stage both the image
and recipe before replacing either; if the second commit fails, they
restore the first. These two renames are not jointly atomic: process
termination, power loss, concurrent changes to destinations, or a failed
rollback are not covered by this guarantee. A failed rollback reports the
affected paths and retains the old output backup, when one exists, at the
reported recovery path. Do not assume a two-file transaction survived a
crash.

`inspect.compression` reports the sensor encoding as `uncompressed`,
`lossy-compressed` or `lossless-compressed`. Lossless Bayer full-frame L
and APS-C M are supported; reduced-size YCbCr full-frame M/S and APS-C S
return an `unsupported` error. See [supported formats](formats.md).

## Recipes and output options

The default recipe is `<raw>.rawsmith.json` next to the RAW. It stores
only settings that differ from the defaults, plus the rawsmith version in
`rawsmith`. For example:

```json
{
  "exposure": 0.35,
  "highlights": -40.0,
  "shadows": 35.0,
  "vibrance": 20.0,
  "hsl": { "blue": { "saturation": -15.0 } },
  "rawsmith": "0.2.0"
}
```

Version 0.2.0 uses a stronger base curve, preserves colour through tone
mapping and measures denoise when unset. Version 0.4.0 removes luminance
grain wherever `denoise` resolves above 10 and extends `defringe` to
coloured specks. Existing sparse recipes render
under these current defaults; explicit values remain exact. Their writer
version does not pin old rendering. `recipe_compatibility` reports
`written_version`, `rendering_version`, `policy: "current_defaults"` and a
`note` when the writer version differs or is absent. It is null without an
input recipe (including `edit --reset`). For `edit`/`auto` it describes the
input recipe; the saved recipe records the current version. `render` and
`compare` never rewrite it. Use the original executable for the original
look; separate recipes alone do not pin a version's defaults.

### Reusing the developed frame

Everything up to noise reduction depends only on the RAW and on
`temperature`, `tint`, `lens_corrections`, `defringe` and `denoise`.
`render`, `edit`, `auto` and `compare` keep that developed, scene-linear
frame on disk and start later renders of the same photo from it; changing
any other setting, including crop and rotation, reuses it. The stored
values are the exact float bits development produced, so outputs and
reports are byte-identical with and without reuse. Reports say what
happened in `scene_reuse`: `reused`, `stored` or `off`.

A frame is identified by the sensor data, those settings and the content
of the running executable, so another build never reads it. Frames live in
`rawsmith-scenes-<user>` under the system temporary directory, private to
the user (a directory another account owns or can write is not used). A 33
MP frame takes about 390 MB; the most recently used 6000 MB are kept and
frames unused for a day are removed. `RAWSMITH_CACHE_DIR` moves the
directory and `RAWSMITH_CACHE_MB` sets the limit, with `0` turning reuse
off. Frames under a megapixel are not stored. A stored frame carries a
checksum of its pixels; one that fails it, or any failure to read or write,
develops the photo as usual. Cleaning up removes only files this cache
wrote, so the directory may hold other things.

An explicit `render --recipe FILE` requires that file to exist, so a
misspelled path cannot silently export an unedited picture. `edit` and
`auto` can create a new recipe at that path. Without `--recipe`, a missing
sidecar means the default recipe. `--size` must be at least 1 pixel and
`--quality` is 1–100 (default 92, full chroma resolution); invalid values
are reported as JSON errors.

`render`, `edit` and `auto` report `recipe_file`: the path read by `render`
(or `null` if no default sidecar was read), and the path saved by
`edit`/`auto`. `recipe` contains the effective settings, including any
`--set` trial. `render` never saves a recipe, even with trial settings.

Add `--brief` to `render`, `edit` or `auto` for the editing loop. It keeps
`output`, encoded `output_bytes`, `quality`, `quality_limited_by`,
`output_replaced`, `output_color_space`,
`recipe_file`, `recipe_saved`, `white_balance`, `region`, sensor clipping and
its scope, and `noise_reduction.strength` / `source`. `stats` keeps `width`,
`height`, all seven tone percentiles (`stats.tones.p1` through `p99`),
`clipped_highlights_pct` (pure white), `channel_clipped_pct` (any channel),
`crushed_shadows_pct`, `neutral_cast` and `zones`. `warnings` includes input
recipe version mismatches and unresolved/unreachable/unavailable auto bounds.
`auto.decisions` has one shortened line per decision (first clause, at most
120 characters plus an ellipsis and any failure qualifiers from later clauses).
Omit `--brief` for complete decisions, recipe settings, histogram, hue/chroma
statistics and noise diagnostics. Full reports also include these export fields.
`auto` analyses the photo before applying any `--set`; when assignments are
given, the last decision lists them and says the earlier decisions describe
the proposal before them.

For recipe-only `edit`/`auto`, both reports have `output_bytes: null` and
`quality: null`; brief `output` and `stats` are also `null`. Brief reports
still include `recipe_saved: true` and the saved path.
Brief `render` reports `recipe_saved: false`, including with `--set`.

`rawsmith schema` lists every parameter with its range and meaning:
white balance (`temperature`, `tint`), tone (`exposure`, `contrast`,
`highlights`, `shadows`, `whites`, `blacks`, `clarity`, `dehaze`, `curve`),
colour (`vibrance`, `saturation`, eight `hsl` bands, shadow/highlight
`grade`), detail (`sharpen`, `denoise`, `defringe`), local adjustments (`vignette`,
`gradients`, `masks`), and geometry (`rotate`, `crop`, `lens_corrections`).

`denoise` defaults to `null`: omitted/null means native noise measurement
with an ISO prior, shared by default rendering and `auto`. `auto` preserves
an explicit amount. Any number in 0–100 is exact, including 0 and the former
default 10. `noise_reduction.strength` reports the actual amount used;
`source` is `recipe`, `measured` or `iso_prior`. `native_sqrt_noise` reports
the native quiet-patch proxy, or null if unavailable/not sampled;
`iso_prior` is null for explicit amounts. Measurement uses the full active
sensor, independent of crop, region and output size.
Use `--set denoise=null` to return to measurement. To increase a measured
amount, read `noise_reduction.strength` and set a number first:
`denoise+=5` rejects a null setting because there is no numeric recipe value.

Neutral thin-edge false-colour protection is automatic in demosaicing and
requires no recipe setting. `defringe` (0–100, default 0) adds stronger
purple, blue and pink fringe suppression after colour conversion. Setting it to 0
turns off only the additional filter; it does not turn off the demosaic fix.
Inspect full-resolution colour detail before saving additional suppression.

`--set path=value` sets a value; `path+=value` and `path-=value` adjust
an existing numeric value. For gradients, `--set 'gradients+={"from":[0.5,0],"to":[0.5,0.5],"exposure":-0.6}'`
appends **one JSON object**; `--set gradients-=1` removes the second filter
(zero-based index). `gradients=[...]` still replaces the whole list and
`gradients=[]` clears it. Assignments run in command-line order; removal
indices refer to the current list after earlier assignments. An append beyond
four filters fails, even if a later assignment would remove one. Arrays on
append, repeated operators (`gradients++=`), negative/fractional indices and
out-of-range indices are errors; no recipe or output is changed.
`render --set` tries changes without saving the
recipe. `edit --set` saves them; `edit --reset` starts from defaults.
Auto computes a fresh tone/colour base and clears manual gradients, masks and
vignette; keep a separate recipe to preserve a finished manual version.
Write previews to separate files to compare rounds. To keep several edits,
use a separate `--recipe FILE` for each, consistently across `auto`,
`edit` and `render`.

Outputs are replaced; RAWs never. Use a new output name to keep a version.
`output_replaced` is `true` when an existing output name was replaced,
`false` for a new output, and `null` when no image was written. It includes
replacing a symlink (even a dangling one); the link target is not written.
This field concerns the output image, not the saved recipe.

`--size` sets the longest edge by area-resizing the finished full-resolution
render, after noise reduction, tone and sharpening. It does not reduce the
resolution of those stages or their working memory. `--region left,top,right,bottom` uses
fractions of the upright, cropped picture and renders that region at full
resolution; it cannot be combined with `--size`. **`edit --region` saves the
settings for the whole picture, while the output image contains only that
region. It is not a trial.** Its full and brief reports include
`recipe_saved: true`, `region` and a `note` explaining this. The same applies
to `auto --region`. Use `render --region --set ...` for an unsaved detail trial.
Output is sRGB JPEG or 16-bit PNG, selected by the output extension.

JPEG quality defaults to **92**, with full chroma resolution: the best
out-of-the-box choice for almost everything. `--quality 1..100` is available
on `render`, `edit`, `auto` and `compare`; lower it for smaller files.
For maximum fidelity or further editing, export `.png` (16-bit lossless).
Quality and byte limits are export settings and are never saved in recipes.
Full and brief reports include `output_bytes` and the actual JPEG `quality`
(`null` for PNG or no image). Extracted camera JPEGs report `quality: null`
because they are copied unchanged and their encoder quality is unknown.

`render`, `edit` and `auto` accept `--max-bytes SIZE` for JPEG output only
(default: no limit). Use it with `--size` for web or file-size targets:

```sh
rawsmith render photo.ARW -o web.jpg --size 2000 --max-bytes 1.5MB --brief
rawsmith render photo.ARW -o smaller.jpg --size 2000 --quality 85 --brief
```

SIZE accepts positive plain integer bytes or decimal K/KB (1000 bytes) and
M/MB (1,000,000 bytes), case-insensitively. Fractional suffix values must
resolve to whole bytes. The image is rendered and quantised once; only the
JPEG encoder is bisected from `--quality` (default 92) downward to a floor
of 60 to find the highest fitting quality. Because JPEG sizes can be
non-monotonic, untested higher qualities are checked before reporting a
result or failure; this can require additional encodes, never another render.
With a byte limit, `--quality` must be at least 60. If lowered, full and brief reports include
`quality_limited_by: "max_bytes"` (otherwise `null`). If no quality at or
above the floor fits the limit, the command returns a JSON error giving the smallest
size reached and suggesting `--size`; no image or recipe is written and
existing files remain unchanged. PNG output, missing output, invalid suffixes
and overflowing/zero limits are rejected. Selection and encoded bytes are
deterministic under the same conditions as ordinary exports.

JPEG embeds a compact ICC v2 sRGB
profile; PNG declares sRGB with its standard sRGB chunk. This changes
metadata only, not pixels. Developed files contain no camera metadata.
`output_color_space` is `"sRGB"` when a developed image was written and
`null` otherwise. [Profile source and CC0 licence](../assets/profiles/README.md).

## Local masks

`masks` holds up to **eight** local entries, in addition to the four legacy
`gradients`. Each entry multiplies its shape by an optional luminance range,
then applies its adjustments. All positions refer to the **finished rotated,
cropped frame**, with [0,0] at top left. Reposition after changing framing.
Existing `gradients` recipes keep their original arithmetic and rendering.

```sh
# Lift a subject inside a feathered ellipse.
rawsmith render photo.ARW --set 'masks+={"shape":"radial","center":[0.6,0.5],"radii":[0.18,0.22],"feather":0.6,"exposure":0.7}' -o trial.jpg --size 1400
# Select only bright sky within a top-down linear gradient, excluding dark branches.
rawsmith render photo.ARW --set 'masks+={"from":[0.5,0],"to":[0.5,0.6],"luma":[0.75,1],"luma_feather":0.08,"exposure":-0.4}' -o sky.jpg --size 1400
# View this same trial selection, replacing the trial list so its index is 0.
rawsmith render photo.ARW --set 'masks=[{"from":[0.5,0],"to":[0.5,0.6],"luma":[0.75,1],"luma_feather":0.08,"exposure":-0.4}]' --show-mask 0 -o mask.png --size 1400
```

`masks=[...]` replaces the list; `masks=[]` clears it; `masks+={...}` appends
one object; `masks-=INDEX` removes a zero-based current-list index. Assignments
run in order. Invalid entries, excess appends and bad indices fail before
saving either recipe or image, even if a later assignment would remove them.
`render --set` is a trial; use `edit --set` to save the same assignments.
Entries are sparse. Unknown fields and shape-inappropriate fields are errors.
Set entry fields by replacing the list or removing/appending an entry; indexed
`--set masks.0.exposure=...` assignments are not supported.

| Field | Range / meaning |
|---|---|
| `shape` | `linear` (default) or `radial` |
| `from`, `to` | Linear [x,y] fractions 0–1, at least 0.02 apart; full at/beyond from, smoothly zero at to |
| `center`, `radii` | Radial centre 0–1; semi-axes 0.01–2, fractions of width and height before ellipse rotation |
| `rotation` | Radial clockwise degrees −180–180 (default 0), rotated in pixel space |
| `feather` | Radial 0.01–1 (default 0.5); fade inside the outer ellipse over this fraction of its radius |
| `invert` | Invert the shape to affect its outside; without shape geometry invert the luma/colors selection |
| `luma` | [low,high] display lightness 0–1, low < high; omit shape geometry to select luminance alone |
| `luma_feather` | 0.02–0.5 (default 0.05), smooth transitions outside low/high; bounds 0/1 include black/white fully |
| `colors` | HSL band names (`red` … `magenta`, no repeats); selects colourful pixels whose hue lies in those bands; omit shape geometry to select colour alone |
| `exposure` | −3–3 EV |
| `saturation` | −100–100, Oklab chroma multiplier as for the global control |
| `contrast` | −100–100 offset to global contrast, clamped to its range; blends the corresponding filmic and colour rendering |
| `shadows`, `highlights` | −100–100 offsets using the same edge-aware log-luminance base and response as the global controls |

`rawsmith schema` includes these ranges and meanings in `local_masks.fields`.
Luminance selection uses Oklab display lightness from global exposure and
legacy gradients through the global filmic curve, **before** new local edits,
vignette, dehaze, global shadow/highlight adjustments, black/white points,
point curve, colour adjustments and sharpening. All entries share this
immutable selection, so one local edit cannot move another's threshold.
A native-resolution luminance-guided refinement suppresses flat-patch grain
while retaining thin high-contrast occluders. Soft thresholds and clamped
weights avoid selection overshoot; inspect actual edges at 100%.

Colour selection reads hue and chroma from the same Oklab guide. Band
membership uses the HSL band weights, so adjacent named bands cover the hues
between them completely; it fades in between chroma 0.015 and 0.045, leaving
greys and near-neutral pixels out, since their hue is mostly noise. A joint
guided filter (radius six native pixels, guided by lightness) smooths hue
noise while keeping the selection's edges on lightness edges. Shape, luma and
colour weights multiply. Everything of the selected colour is selected, not
one object.

Masked exposure offsets add in stops; masked shadows/highlights share one
base layer after that exposure. They precede vignette and the global local-tone
stage. Contrast entries blend in list order from the global curve towards
the global-plus-offset curve, including its colour separation. Saturation
multipliers combine after global colour adjustments. Zero adjustment values
are neutral. For extreme overlapping masks, each scene gain is bounded to
±32 stops and to a largest absolute RGB channel of 65536, using one common
gain to preserve chromaticity. This prevents valid stacked entries overflowing
to infinity/NaN and exporting black; ordinary photographic settings stay
below these safety bounds. Raising exposure/shadows can still reveal or brighten existing
noise; these masks do not replace denoising. Narrow brightness thresholds can
select texture as well as objects. At soft out-of-focus edges, a brightness
selection can reshape the blur; prefer a radial without luma for a blurred
corner distraction. A blown sky cannot regain detail and can
turn grey when darkened too much.

`render --show-mask N` shows the **masks** entry N, white selected, black
excluded, without adjustments, sharpening or tone mapping of the weights.
`--size` resizes the native mask last; `--region` extracts a native region after
full-frame mask refinement. Full and brief JSON include a `show_mask` diagnostic
marker; statistics describe the greyscale weights, not a developed photo.
This never saves a recipe and invalid indices preserve existing files. Indices
refer to the effective list after all trial assignments; an appended entry has
index equal to the previous list length.
The diagnostic supports sRGB JPEG and 16-bit PNG; PNG is preferable for checking
weights. There are no local temperature/tint controls, brushes or semantic
subject masks. White balance remains a global pre-demosaic operation.

## Comparing versions

```sh
# Camera JPEG on the left, saved edit on the right
rawsmith compare photo.ARW -o comparison.jpg
# Camera alone, upright and converted to sRGB when its colour is established
rawsmith compare photo.ARW --panel camera -o camera.jpg --size 1400
# Earlier recipe versus the current saved edit, in this order
rawsmith compare photo.ARW --panel recipe:earlier.json --panel saved -o rounds.png
# Camera beside a recipe kept elsewhere: the same --recipe as auto, edit and render
rawsmith compare photo.ARW --recipe edits/photo.json -o comparison.jpg
# The same part of camera and edit at full resolution, side by side
rawsmith compare photo.ARW --region 0.4,0.3,0.55,0.45 -o detail-cmp.jpg
# Camera, finished default and another recipe
rawsmith compare photo.ARW --panel camera --panel default --panel recipe:alternate.json -o versions.jpg --size 1000
```

`compare` writes one unlabelled row with a two-pixel neutral grey separator
between panels. Repeat `--panel` in left-to-right order, selecting `camera`,
`saved`, `default` or `recipe:FILE`. With no `--panel`, it selects `camera`
then `saved`; a single explicit panel is also allowed, with no separator. `saved`
reads `--recipe FILE` when given (it must exist), otherwise `<raw>.rawsmith.json`,
using defaults when that sidecar is missing. `default`
always uses the finished default recipe. A missing explicit `recipe:FILE`
is an error. The command never saves recipes, and refuses to overwrite the
RAW, its default sidecar, the `--recipe` file, or any selected recipe
(including file aliases).

Developed panels include `noise_reduction` and `recipe_compatibility` with
the same meanings as render reports. Camera panels have no noise measurement
because their JPEG is not developed by rawsmith.

Every panel has a `look`: lightness percentiles `p1`, `p5`, `p50`, `p95`,
`p99` and `spread`, mean chroma, pure white and pure black percentages, and
`colours`, the share, chroma and lightness of each colour family holding at
least 5% of the colourful pixels, with its mean Oklab `hue` in degrees. When the row has a camera panel with
established colour, each developed panel in the camera's framing also has
`losses_against_camera`: plain sentences for what that version gave up
against the camera's rendering — darkest tones lifted by five points or
more, lightness spread below 80% of the camera's, one point more pure white
or two more pure black, a colour family at under 80% of its chroma (and
whether it also became paler), one whose mean hue moved by eight degrees or
more (named by the neighbouring band it turned towards), or one that nearly
disappeared. A colour at over 150% is reported as well. These are measurements to check by eye, not
faults: fog is meant to be soft. An empty list means none of these was
measured, not that the edit is better. Panels with another framing have no
such list.

Every panel is rotated upright using the RAW's orientation, including the
camera JPEG; embedded JPEG orientation metadata is ignored. Orientations
other than 1, 3, 6 and 8 are rejected. The camera then receives the first
non-camera panel's recipe rotation and crop. Every recipe panel retains
its own rotation and crop. If those differ, `framing_matched` is `false`
and `warnings` explains the difference. A camera-only row uses neutral
framing. This matches normalized crop/rotation, not camera lens corrections
or a camera JPEG with a different original field of view.

`--size` defaults to 1400 and bounds **each panel's** longest edge, not the
whole row. Panels shrink proportionally to the smallest bounded panel
height, without upscaling; widths are rounded to whole pixels. JPEG uses
`--quality 1..100` (default 92, full chroma resolution); lower for smaller
files. `.png` gives 16-bit output. The camera decoder accepts 8-bit RGB
or greyscale JPEGs, with a 128 MiB decoded-buffer limit. Images carry no camera metadata.
Encoding and staging complete before the destination is replaced; failures
preserve existing files. Repeated comparisons are deterministic under the
same binary/platform conditions as rendering.

The report contains:

- `output`, `output_bytes`, `quality` (null for PNG), `output_replaced`, `output_color_space`.
- `panels`, in left-to-right order: `kind`, `recipe_file` (null for camera,
  default, or a missing saved sidecar), final `width` and `height`,
  `framing` (`crop`, `rotate`), and `color_space`.
- Camera panels also contain `color_declaration`: `source`
  (`embedded-jpeg-icc`, `embedded-jpeg-exif`, `raw-exif`, or `none`) and
  the declared `color_space`. `color_space` on the panel describes its final
  pixels; `color_conversion` is `"Adobe RGB (1998) to sRGB"` when converted,
  otherwise null.
- Whole-row `width`, `height`, `separator_pixels`, `raw_orientation`,
  `camera_framing_panel` (zero-based index; null without a camera or without
  a non-camera reference), `framing_policy`, `framing_matched`, `warnings`.

Embedded JPEG declarations take precedence over the RAW: ICC first,
then JPEG EXIF, then RAW EXIF when the JPEG has no colour declaration.
EXIF is read through IFD0's ExifIFD pointer (`0x8769`): ColorSpace (`0xa001`)
and its InteropIFD pointer (`0xa005`) to InteropIndex (`0x0001`). These describe
the camera preview's encoding, not the sensor mosaic's colour space.
EXIF ColorSpace 1 / interoperability R98 (including ColorSpace 65535 with
R98) and the exact bundled sRGB ICC profile establish sRGB. Other ICC
profiles remain `ICC (unverified)`; they
are not converted or overridden by EXIF or RAW metadata.

Interoperability R03 with ColorSpace 65535 (uncalibrated), or without
ColorSpace, declares `Adobe RGB (1998)`. Its camera pixels are converted
before framing/resizing: gamma 563/256 to linear Adobe RGB, a D65 primary
matrix to linear sRGB, then the standard sRGB transfer function. Colours
outside sRGB are clipped in linear destination RGB. The declaration remains
`Adobe RGB (1998)` in the report, with final panel `color_space: "sRGB"`
and `color_conversion: "Adobe RGB (1998) to sRGB"`.

Uncalibrated EXIF without an interoperability index remains `uncalibrated`;
missing or conflicting declarations are `unknown`. These and unverified
ICC profiles keep the
warning path: camera pixels remain unconverted, `output_color_space` is
`mixed-unmanaged`, and the row has no ICC profile or PNG sRGB chunk. For
established or converted sRGB, no colour warning is emitted; the output is
`sRGB`, tagged like any developed export (JPEG ICC / PNG sRGB chunk).

## Extracting a camera JPEG

`extract-jpeg RAW -o camera.jpg` copies the embedded JPEG bytes unchanged,
including any camera metadata. It does not rotate pixels, add a colour
profile or strip metadata, and does not report `output_color_space`.
`output_replaced` follows the same replacement rule as developed images.

`raw_orientation` is the RAW's numeric EXIF orientation.
`rotation_degrees_clockwise` describes how to rotate the stored pixels
for comparison with an upright rawsmith render: 1 → 0°, 3 → 180°,
6 → 90°, 8 → 270°. `needs_rotation` is `false` for 1, `true` for 3/6/8.
Both derived fields are `null` for other orientations; inspect the original
orientation for mirroring or unsupported values. The orientation is taken
from the RAW, not parsed from the embedded JPEG. A viewer may already apply
orientation metadata in the JPEG: check its displayed geometry and avoid
rotating twice.

Render reports echo `region` as `[x0,y0,x1,y1]` in the rotated/cropped
frame, or `null` for the whole frame. Output `stats` describe those pixels.
Clipping counts every output pixel before file dithering: encoded sRGB
values ≥0.998 count as at the ceiling. `clipped_highlights_pct` counts all
three channels; `channel_clipped_pct` any channel;
`channel_clipped_pct_rgb` counts R/G/B separately. Pure-white zero does not
rule out damaged highlight colour.

`inspect.exposure.saturated_pct` counts saturated photosites per colour over
all complete 2×2 CFA cells in the active sensor area, rather than the old
1-in-16 cell sample. `saturated_cells_pct` counts cells with that colour
saturated (either green site); `sensor_clipped_pct` counts their union.
All use ≥98% of black-subtracted range, before hot-pixel repair and white
balance. Green photosites have twice the cell denominator, so the union
can exceed the sum of `saturated_pct`. Render's `sensor_clipped_pct` is now
exactly the same union as inspect's, and `sensor_clipped_scope` is
`"full_active_area"`: it does not describe the cropped frame or region.
Neither clipping count measures recovered detail or proves correct colour.

Preview statistics describe the rendered preview. Small clipped areas can
average away at a small size, so clipping percentages may differ slightly
from those in the full-resolution export. Read the final export's report
as well as checking full-resolution detail.

With the same executable binary on the same platform (OS/runtime versions
and CPU architecture/features), identical RAW bytes, effective recipe
settings and render options (`--size`, `--region`, output format and JPEG
quality and `--max-bytes`) produce bit-identical pixels, JPEG/PNG bytes and
report statistics,
regardless of Rayon thread count or scheduling. Report timing and paths are
not part of this guarantee. A matching version number alone, different
builds or different platforms do not guarantee bit-identical results.

For the complete editing workflow and guidance on reading statistics, see
[the editing skill](../skill/rawsmith/SKILL.md). Command-specific options
are available through `rawsmith <command> --help`.
