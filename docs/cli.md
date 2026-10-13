# CLI reference

Successful subcommands print JSON on stdout. Errors print JSON on stderr
with exit status 1 and nothing on stdout. The error object contains
`error.kind` and `error.message`; I/O errors name the relevant path.
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

`inspect.compression` reports the sensor encoding as `uncompressed` or
`lossy-compressed`. Unsupported encodings (including lossless-compressed
ARW) still return an `unsupported` error rather than a successful report.

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
  "rawsmith": "0.1.1"
}
```

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
`grade`), detail (`sharpen`, `denoise`), local adjustments (`vignette`,
`gradients`), and geometry (`rotate`, `crop`, `lens_corrections`).

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

`--size` sets the longest edge. `--region left,top,right,bottom` uses
fractions of the upright, cropped picture and renders that region at full
resolution; it cannot be combined with `--size`. Output is sRGB JPEG or
16-bit PNG, selected by the output extension. JPEG embeds a compact ICC v2 sRGB
profile; PNG declares sRGB with its standard sRGB chunk. This changes
metadata only, not pixels. Developed files contain no camera metadata.
`output_color_space` is `"sRGB"` when a developed image was written and
`null` otherwise. [Profile source and CC0 licence](../assets/profiles/README.md).

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
