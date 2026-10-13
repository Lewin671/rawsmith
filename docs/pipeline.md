# How rawsmith develops a photo

The pipeline is fixed: the same stages run in the same order for every
photo, and a recipe only changes their settings. That keeps results
reproducible and the parameter space small enough for an assistant to reason
about. Each stage lives in one module under `src/`.

Internally the picture is 32-bit float RGB. Until tone mapping it is
*scene-linear*: values are proportional to light, 0.18 is middle grey, and
highlights may exceed 1.

## 1. Decode — `arw.rs`, `tiff.rs`

An ARW file is a TIFF container. A bounds-checked reader walks its
directories and extracts the sensor mosaic plus what is needed to interpret
it: black and white levels, the camera's white balance, the active area, the
Bayer layout, orientation, and the lens-correction tables the camera stored.

Two encodings are supported: uncompressed 14-bit, and Sony's lossy
compression (11-bit values through a tone curve, 7-bit deltas in 16-sample
blocks). Decoded samples are verified to be identical to LibRaw's on real
files (see [benchmark.md](benchmark.md)). LibRaw is only a test reference;
rawsmith does not link or call it.

## 2. White balance and demosaic — `demosaic.rs`

Samples are black-subtracted, scaled so sensor saturation is 1, and
multiplied by the white balance gains. Gains come from the camera's as-shot
values, or from `temperature` / `tint` through the camera's colour matrix.

Full-resolution demosaicing is edge-directed: green is interpolated along
the direction of least change with a Laplacian correction from the
co-located colour (Hamilton–Adams, with soft direction weights), then red
and blue from interpolated colour differences. Previews and exports use the
same native-resolution demosaic. Automatic candidate analysis alone uses an
approximate reduced scene: each 2×2 Bayer tile becomes one pixel. This
analysis image is not an output preview.

## 3. Highlight reconstruction — `demosaic.rs`

After white balance each channel has a different sensor limit (its WB gain).
Saturation is detected against those limits, not against a common RGB value.
A missing channel is estimated using ratios measured on bright, intact
pixels bordering saturated regions. Ratios preserve chromaticity as
brightness increases, unlike additive cube-root offsets. The bordering
colour is a prior: different lamps or daylight regions can disagree.

Reconstruction blends in over the last tenth of sensor range (90–98%);
near-clipped channels progressively stop being trusted. A colour prior
that disagrees with surviving-channel ratios or predicts less than a
clipped channel's observed lower bound loses authority. With two reliable
survivors, their measured colour is kept and only the missing channel is
estimated conservatively from them. With an untrusted prior and only one
reliable survivor, colour fades continuously towards neutral. Independently
keeping each clipped bound under an incompatible colour prior would invent
a hue. Variation in the border's colour ratios also lowers trust: mixed
illuminants do not establish the patch's missing colour. A consistent
coloured reference remains reconstructable. The existing 2% clipping margin
sets the tolerance for these consistency checks. This deliberately favours
neutral highlights over unsupported colour.

As the last channel approaches its limit, colour fades continuously to a
neutral highlight. Its endpoint is at least as bright as the prior-based
estimate at the last-channel limit, including WB gains, so a fully clipped
core does not become darker than its reconstructed boundary. This estimates
plausible colour and roll-off, not lost detail. With no bright, intact
reference the prior defaults to neutral. A wrong prior consistent with the
remaining channel can still be wrong; optical and fine demosaic fringes
are not resolved by this stage.

Sensor clipping reports use every complete 2×2 CFA cell in the full active
area, before hot-pixel repair and WB, at 98% of black-subtracted sensor
range. `inspect.exposure.saturated_pct` counts photosites of each colour (two
green sites per cell); `saturated_cells_pct` counts cells with either site
of that colour clipped. `sensor_clipped_pct` is the union of clipped cells,
identical in inspect and render. It need not be below the sum of the
photosite percentages: their denominators differ. Cropping, rotation,
`--region`, resizing and WB do not change this full-active-area statistic;
render reports identify its scope explicitly.

## 4. Lens corrections — `lens.rs`

Sony cameras store per-shot radial tables for distortion, lateral chromatic
aberration and vignetting. rawsmith applies them in a single cubic
resampling pass (each colour channel at its own radius) and scales the
result to fill the frame exactly, matching the camera's own JPEG geometry.

## 5. Colour — `color.rs`

White-balanced camera RGB is converted to the working space, linear
Rec.2020, with a per-camera matrix. The matrix is sensor characterisation
data; cameras without one are rejected rather than rendered with wrong
colour.

## 6. Noise reduction and geometry — `pipeline.rs`

Chroma noise is smoothed in a square-root encoding where sensor noise is
roughly uniform. A native-resolution guided filter follows luminance edges
and removes grain. Above `denoise` 25, a gradually blended quarter-resolution
luminance-guided chroma base correction also suppresses correlated colour
blotches over a 48-native-pixel radius. A shared boundary mask from the
quarter-resolution chroma and luminance guides protects strong colour
discontinuities across the filter's support,
including boundaries with little luminance contrast. Above 50, luminance
smoothing joins in: square-root Rec.2020 luminance blends gradually toward
the chroma-cleaned reconstruction, then is filtered using that reconstruction as its guide.
This avoids restoring matrix-correlated colour noise as luminance grain,
with lower regularisation than the former sqrt-RGB luminance proxy. Up to
50, chroma changes preserve linear Rec.2020 luminance exactly; higher values
set final luminance from the filtered estimator through a common RGB gain.
Orientation, `rotate` and `crop` follow. Output rendering keeps native resolution here;
the reduced automatic analysis path remains an approximation.

## 7. Tone — `tone.rs`

- `exposure` scales scene light.
- `dehaze` removes airlight with the dark channel prior.
- `highlights`, `shadows` and `clarity` work on a base/detail split of log
  luminance. The base layer is an edge-aware (guided) smoothing computed at
  reduced size and applied at full resolution, so large regions can be
  brightened or darkened without halos and without flattening texture.
- A filmic sigmoid maps scene light to display range: middle grey stays
  put and both ends roll off smoothly. An upper Hermite shoulder joins the
  sigmoid halfway to its scene white point and reaches white with zero
  derivative, avoiding a hard per-channel cut-off.
  Shadows and midtones keep their hue under the curve; highlights take it
  per channel, so bright colours fade towards yellow and white the way film
  and camera JPEGs do. `contrast` changes the slope, `whites` the white
  point.
- `blacks` and the optional point `curve` act on display values.

## 8. Colour adjustments — `adjust.rs`

`vibrance`, `saturation`, the eight `hsl` bands and the two-way colour
`grade` operate in Oklab, where hue, chroma and lightness can be changed
independently.

## 9. Output — `output.rs`

Colours outside sRGB are brought inside by reducing chroma at constant
lightness and hue. The result is encoded as sRGB, sharpened on encoded
luminance with an unsharp mask limited to the local range (edges steepen,
they do not halo), clamped, then dithered to 8 bits for JPEG or written as
16-bit PNG. Output files carry no EXIF, GPS or serial numbers.

Only after tone, colour, gamut mapping, encoded-luminance sharpening and
clamping is the finished sRGB image area-averaged to `--size`. This gives the
same noise, sharpness and tone as resizing a full render with the same area
filter, rather than changing spatial radii or processing averaged linear
pixels. Previews therefore do full-resolution work and use full-resolution
working memory. `compare` uses this path for developed panels too.

`examples/quality.rs --check` compares a noisy synthetic RAW at several sizes
against the downscaled full render, with noise standard deviation, edge
acutance, mean, p1/p50/p95/p99 and maximum channel error. Its tolerances are
1% relative for noise/acutance and 0.001 in encoded 0–1 units for mean,
percentiles and channel error; the current path is exactly equal before
output quantisation. This guarantee is relative to the same area resize,
not an arbitrary viewer filter or separately compressed JPEG. PNG rounding
and JPEG quantisation/compression can introduce additional differences.
The high-ISO-like synthetic scene separately checks both low-frequency
chroma axes (at least 50% RMS removal), fine texture projection (at least 90%
retained) and no amplification of flat-area luminance noise at strengths 60,
85 and 100. These are deterministic synthetic floors, not claims about every
real camera or photo. Fine grain and very large colour clouds may remain.

Statistics describe the resulting output size: full-resolution statistics
can legitimately differ from statistics of its resized version.

## Automatic starting point — `auto.rs`

`rawsmith auto` does not predict settings from the raw data. It renders
candidates on a small copy and measures them: exposure is searched so the
bright part of the picture reaches a target lightness (near white for
neutral highlights such as snow and fog, lower for colourful ones such as a
sunset sky) without washing out midtones or adding clipping; shadows,
highlights, contrast, black point and vibrance follow from the measured
tone distribution. Every decision is reported in words and saved as
ordinary recipe values.

## Clipping in render reports — `stats.rs`

Output clipping counts every rendered pixel before encoding/dithering:
`clipped_highlights_pct` requires all three encoded sRGB channels ≥0.998,
`channel_clipped_pct` requires any one, and `channel_clipped_pct_rgb` gives
R/G/B separately on the same pixel set. Other tone and colour statistics
use a regular grid of at most about 400k samples. Output statistics apply
to the requested size and region; `region` echoes the requested normalized
coordinates in the rotated/cropped frame (or null for the whole frame).
Output clipping and sensor clipping answer different questions. A low
pure-white count cannot establish that partly clipped highlights have
correct colour or that sensor-saturated detail was recovered.
