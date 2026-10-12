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
and blue from interpolated colour differences. When the requested output is
at most half the sensor size, each 2×2 Bayer tile becomes one pixel instead:
exact, artefact-free and four times cheaper — this is what makes previews
fast.

## 3. Highlight reconstruction — `demosaic.rs`

After white balance the channels saturate at different levels, so a blown
area keeps some channels and loses others; left alone it turns magenta. A
lost channel is estimated from the surviving ones plus the typical offset
between them, measured on unclipped pixels bordering the blown area.
Where all three channels are gone nothing can be recovered and the area
becomes neutral white.

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

Chroma noise is smoothed with a guided filter that follows luminance edges,
in a square-root encoding where sensor noise is roughly uniform; luminance
smoothing joins in the upper half of the `denoise` range. Orientation,
`rotate` and `crop` follow, then the resize to the requested size.

## 7. Tone — `tone.rs`

- `exposure` scales scene light.
- `dehaze` removes airlight with the dark channel prior.
- `highlights`, `shadows` and `clarity` work on a base/detail split of log
  luminance. The base layer is an edge-aware (guided) smoothing computed at
  reduced size and applied at full resolution, so large regions can be
  brightened or darkened without halos and without flattening texture.
- A filmic sigmoid maps scene light to display range: middle grey stays
  put, sensor saturation reaches white, and both ends roll off smoothly.
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
lightness and hue. The result is sharpened with an unsharp mask limited to
the local range (edges steepen, they do not halo), encoded as sRGB,
dithered to 8 bits for JPEG or written as 16-bit PNG. Output files carry no
EXIF, GPS or serial numbers.

## Automatic starting point — `auto.rs`

`rawsmith auto` does not predict settings from the raw data. It renders
candidates on a small copy and measures them: exposure is searched so the
bright part of the picture reaches a target lightness (near white for
neutral highlights such as snow and fog, lower for colourful ones such as a
sunset sky) without washing out midtones or adding clipping; shadows,
highlights, contrast, black point and vibrance follow from the measured
tone distribution. Every decision is reported in words and saved as
ordinary recipe values.
