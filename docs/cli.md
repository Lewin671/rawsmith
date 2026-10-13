# CLI reference

Successful subcommands print JSON on stdout. Errors print JSON on stderr
and return a nonzero exit code. `--help` and `--version` print plain text.

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

`--size` sets the longest edge. `--region left,top,right,bottom` uses
fractions of the upright, cropped picture and renders that region at full
resolution; it cannot be combined with `--size`. Output is sRGB JPEG or
16-bit PNG, selected by the output extension.

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
