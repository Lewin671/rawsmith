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
mapping and measures denoise when unset. Existing sparse recipes render
under these current defaults; explicit values remain exact. Their writer
version does not pin old rendering. `recipe_compatibility` reports
`written_version`, `rendering_version`, `policy: "current_defaults"` and a
`note` when the writer version differs or is absent. It is null without an
input recipe (including `edit --reset`). For `edit`/`auto` it describes the
input recipe; the saved recipe records the current version. `render` and
`compare` never rewrite it. Use the original executable for the original
look; separate recipes alone do not pin a version's defaults.

An explicit `render --recipe FILE` requires that file to exist, so a
misspelled path cannot silently export an unedited picture. `edit` and
`auto` can create a new recipe at that path. Without `--recipe`, a missing
sidecar means the default recipe. `--size` must be at least 1 pixel and
`--quality` is 1–100; invalid values are reported as JSON errors.

`render`, `edit` and `auto` report `recipe_file`: the path read by `render`
(or `null` if no default sidecar was read), and the path saved by
`edit`/`auto`. `recipe` contains the effective settings, including any
`--set` trial. `render` never saves a recipe, even with trial settings.

`rawsmith schema` lists every parameter with its range and meaning:
white balance (`temperature`, `tint`), tone (`exposure`, `contrast`,
`highlights`, `shadows`, `whites`, `blacks`, `clarity`, `dehaze`, `curve`),
colour (`vibrance`, `saturation`, eight `hsl` bands, shadow/highlight
`grade`), detail (`sharpen`, `denoise`, `defringe`), local adjustments (`vignette`,
`gradients`), and geometry (`rotate`, `crop`, `lens_corrections`).

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
purple/blue fringe suppression after colour conversion. Setting it to 0
turns off only the additional filter; it does not turn off the demosaic fix.
Inspect full-resolution colour detail before saving additional suppression.

`--set path=value` sets a value; `path+=value` and `path-=value` adjust
an existing numeric value. `render --set` tries changes without saving the
recipe. `edit --set` saves them; `edit --reset` starts from defaults.
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
resolution; it cannot be combined with `--size`. Output is sRGB JPEG or
16-bit PNG, selected by the output extension. JPEG embeds a compact ICC v2 sRGB
profile; PNG declares sRGB with its standard sRGB chunk. This changes
metadata only, not pixels. Developed files contain no camera metadata.
`output_color_space` is `"sRGB"` when a developed image was written and
`null` otherwise. [Profile source and CC0 licence](../assets/profiles/README.md).

## Comparing versions

```sh
# Camera JPEG on the left, saved edit on the right
rawsmith compare photo.ARW -o comparison.jpg
# Camera alone, upright and converted to sRGB when its colour is established
rawsmith compare photo.ARW --panel camera -o camera.jpg --size 1400
# Earlier recipe versus the current saved edit, in this order
rawsmith compare photo.ARW --panel recipe:earlier.json --panel saved -o rounds.png
# Camera, finished default and another recipe
rawsmith compare photo.ARW --panel camera --panel default --panel recipe:alternate.json -o versions.jpg --size 1000
```

`compare` writes one unlabelled row with a two-pixel neutral grey separator
between panels. Repeat `--panel` in left-to-right order, selecting `camera`,
`saved`, `default` or `recipe:FILE`. With no `--panel`, it selects `camera`
then `saved`; a single explicit panel is also allowed, with no separator. `saved`
reads `<raw>.rawsmith.json`, using defaults when it is missing. `default`
always uses the finished default recipe. A missing explicit `recipe:FILE`
is an error. The command never saves recipes, and refuses to overwrite the
RAW, its default sidecar, or any selected recipe (including file aliases).

Developed panels include `noise_reduction` and `recipe_compatibility` with
the same meanings as render reports. Camera panels have no noise measurement
because their JPEG is not developed by rawsmith.

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
quality 92; `.png` gives 16-bit output. The camera decoder accepts 8-bit RGB
or greyscale JPEGs, with a 128 MiB decoded-buffer limit. Images carry no camera metadata.
Encoding and staging complete before the destination is replaced; failures
preserve existing files. Repeated comparisons are deterministic under the
same binary/platform conditions as rendering.

The report contains:

- `output`, `output_replaced`, `output_color_space`.
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
quality) produce bit-identical pixels, JPEG/PNG bytes and report statistics,
regardless of Rayon thread count or scheduling. Report timing and paths are
not part of this guarantee. A matching version number alone, different
builds or different platforms do not guarantee bit-identical results.

For the complete editing workflow and guidance on reading statistics, see
[the editing skill](../skill/rawsmith/SKILL.md). Command-specific options
are available through `rawsmith <command> --help`.
