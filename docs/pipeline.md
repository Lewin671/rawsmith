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

Three Bayer encodings are supported: uncompressed 14-bit, Sony's lossy
compression (11-bit values through a tone curve, 7-bit deltas in 16-sample
blocks), and lossless Huffman JPEG tiles (T.81 process 14). The latter
reassembles four 14-bit JPEG components into each 512×512 Bayer tile;
full-frame L and APS-C M are verified. Reduced-size YCbCr M/S modes remain
unsupported; see [formats.md](formats.md). Decoded samples are verified to
be identical to LibRaw's on real files (see [benchmark.md](benchmark.md)). LibRaw is only a test reference;
rawsmith does not link or call it.

## 2. White balance and demosaic — `demosaic.rs`

Samples are black-subtracted, scaled so sensor saturation is 1, and
multiplied by the white balance gains. Gains come from the camera's as-shot
values, or from `temperature` / `tint` through the camera's colour matrix.

Full-resolution demosaicing is edge-directed: green is interpolated along
the direction of least change with a Laplacian correction from the
co-located colour (Hamilton–Adams, with soft direction weights), then red
and blue from interpolated colour differences. Previews and exports use the
same native-resolution demosaic. At thin high-contrast structures, a 7×7
binomial low-pass of the measured CFA planes checks for neutral colour
support (channel differences below 2% of the local level). A moderate cast
is also supported when its R/G and B/G ratios agree within 0.025 in all
eight neighbouring CFA tiles; a coloured line changes those ratios. Only
with this support, the measured red/blue photosite and supported channel
ratio constrain green before false colour propagates through the
differences. This retains the supported tint rather than desaturating it.
Measured photosites and clipped green remain available to reconstruction.
This protection is always on, uses the existing green buffer and has no
recipe parameter.

Automatic candidate analysis alone uses an approximate reduced scene:
each 2×2 Bayer tile becomes one pixel. This analysis image is not an output
preview. Strong green variation within a tile can also produce false
colour from different channel sample positions. Neutral tiles use their
four-photosite mean exposure and supported colour ratios; coloured support
in neighbouring CFA tiles protects the wings of genuine coloured detail.
Clipped tiles are excluded. This path adds no frame-sized buffer. Fine
colour near the Bayer sampling limit remains ambiguous; this is a narrow
neutral-structure correction, not a general removal of all false colour.

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
resampling pass (each colour channel at its own radius). Distortion and CA
are interpolated separately at the radius after fill zoom. Positive Sony
CA coefficients sample farther out in the source; red and blue use
`1 + coefficient / 2^21`. The zoom fills the frame using the green-channel
geometry. Correction precedes orientation, crop and output resizing in both
full- and half-resolution paths. It corrects lateral channel displacement,
not longitudinal CA, blooming or demosaic false colour.

## 5. Colour — `color.rs`

White-balanced camera RGB is converted to the working space, linear
Rec.2020, with a per-camera matrix. The matrix is sensor characterisation
data; cameras without one are rejected rather than rendered with wrong
colour.

## 6. Defringe, noise reduction and geometry — `fringe.rs`, `pipeline.rs`

Additional `defringe` (0–100, default 0) reduces thin purple, blue and pink
chroma outliers after colour conversion, before noise reduction, crop and resize.
It requires strong luminance contrast and lower chroma support on both sides
within a square radius of three native-resolution pixels. The reduced
automatic analysis scene uses two half-resolution pixels. Supported purple
surfaces, including their corners, are protected. It changes chroma while
preserving linear luminance, so it cannot reconstruct missing edge shape or
highlight detail. The automatic demosaic protection above runs even at `defringe=0`.
This control adds stronger post-processing for residual fringes; it does
not disable the demosaic fix. The same control also calms false colour on
scattered bright dots: sky seen through a thicket of twigs lands on one or
two photosites at a time, and each dot demosaics into a different pastel.
Where luminance changes at least fourfold within two pixels, a pixel's
chroma moves towards the median chroma of its 5x5 neighbourhood by the
defringe strength (fading in over the next stop of contrast). Colour with
real extent is its own median and is returned bit for bit; linear luminance
is preserved. Additional suppression is off by default because genuinely
purple detail only a few sensor pixels wide can be affected, and a coloured
point one or two pixels across, such as a distant lamp, cannot be told from
a false-colour dot.
Previews resize the filtered native-resolution result; inspect a full-resolution
region before keeping the setting, since resizing can hide residual fringes
and damage to fine colour. Broad purple casts and wide out-of-focus halos
are outside its scope. The filter uses 64-row stripes with original halos rather than
an immutable full frame. At width 7008 and radius 3, the two reusable
buffers hold at most 6.14 MB (5.85 MiB), independent of frame height.

An omitted or `null` `denoise` uses the shared native sensor noise policy in
`noise.rs`, also used by `auto`: quiet-patch square-root residuals plus an
ISO prior, with an honest prior-only fallback when fewer than 16 patches are
usable (details below). Measurement uses the full active sensor before white
balance, lens correction, resizing or recipe framing. An explicit 0–100
amount is obeyed exactly, including 0. Reports expose `noise_reduction`:
`strength`, `source` (`recipe`, `measured` or `iso_prior`),
`native_sqrt_noise` and `iso_prior` (null when not measured/used).
The nullable setting stays unset in saved recipes; it is not silently pinned
to one measurement. Strong shadow lifting can still warrant more denoising.

Chroma noise is smoothed in a square-root encoding where sensor noise is
roughly uniform. A native-resolution guided filter follows luminance edges
and removes grain. Above `denoise` 25, a gradually blended quarter-resolution
luminance-guided chroma base correction also suppresses correlated colour
blotches over a 48-native-pixel radius. A shared boundary mask from the
quarter-resolution chroma and luminance guides protects strong colour
discontinuities across the filter's support,
including boundaries with little luminance contrast.

Luminance grain is removed by wavelet shrinkage of square-root Rec.2020
luminance over four undecimated B3-spline scales. The noise level is the
slider itself (100 stands for a square-root sensor noise of 0.02, of which
0.74 reaches the finest band after demosaicing; coarser bands hold 0.46,
0.22 and 0.12 of that, measured on quiet areas of ISO 100-6400 frames), so
it comes from the sensor measurement and never from the picture's own
detail. In flat areas coefficients below 2.2 noise levels (3.3 in the
finest band, where isolated extremes would survive as specks) are removed
with a non-negative garrote. Where a band's local energy clearly exceeds
its noise the threshold falls to noise squared over signal, which keeps
low-contrast texture, and where the smoother plane has a clear slope an
edge is passing through and its detail is left alone. The threshold fades
in between strengths 10 and 40: ISO 100 frames keep their faint grain and
finest texture, and up to 10 chroma changes preserve linear Rec.2020
luminance exactly. Above 50 the input to this step also blends toward the
chroma-cleaned reconstruction, which avoids restoring matrix-correlated
colour noise as luminance grain. Final luminance is set through a common
RGB gain. Texture weaker than the noise cannot be told from it and is
smoothed with it; settings above the measured amount ask for more smoothing
than the noise needs and take proportionally more fine texture.
Orientation, `rotate` and `crop` follow. Output rendering keeps native resolution here;
the reduced automatic analysis path remains an approximation.

## 7. Tone — `tone.rs`

- `exposure` scales scene light.
- `gradients` apply graduated exposure and saturation adjustments in the
  upright, cropped frame, preserving their original arithmetic.
- `masks` (up to eight, in addition to four legacy gradients) multiply a
  finished-frame linear/radial shape by optional luminance and colour
  selections.
  All entries use one immutable guide after exposure and legacy gradients:
  Oklab lightness of globally filmic-mapped RGB, before new masks and other
  finishing controls. A full-native-resolution self-guided filter (radius
  128 pixels, regularisation 0.0064 in squared lightness) refines that
  lightness before soft range selection. Window means truncate at the frame
  boundary rather than replicating noisy corner pixels, and accumulate in
  f64. A radius-two native detail gate restores strong thin edges that the
  broad luminance regression could attenuate. This suppresses noise-driven mask
  speckle without downsampling away thin branches. Weights are bounded 0–1.
  Shape rotation is in pixel coordinates before dividing by the ellipse's
  semi-axes (fractions of frame width/height). Feather lies inside the outer
  ellipse; inversion affects the shape, or the selection without a shape.
  Colour selection weights each pixel by its HSL band membership times a
  chroma ramp (0.015–0.045 Oklab), then refines that plane with a joint
  guided filter (radius 6, regularisation 0.0004) on the same lightness
  guide; entries naming the same bands share one plane.
  Masked exposure offsets add in stops. Masked shadows/highlights use the
  existing log-luminance base and response, sharing a base across entries
  after masked exposure. Scene gains are bounded to ±32 stops and a largest
  absolute RGB channel of 65536 with a common gain, preventing nonfinite
  pixels from extreme valid overlapping entries. These bounds apply only to
  new mask gains; the legacy path is unchanged. These precede vignette and
  the global local-tone stage.
  Contrast blends the global filmic and colour rendering with the curve at
  global contrast plus offset (clamped), in entry order. Local saturation
  multiplies Oklab chroma after global colour adjustments. White balance stays
  global; no approximate post-demosaic temperature/tint is exposed.
  `render --show-mask N` uses the same selection and geometry at native
  resolution, outputs encoded grey weights, and resizes/extracts a region last.
  It bypasses local adjustments and final sharpening. No-local recipes retain
  the existing operations and pixels; legacy gradient arithmetic is unchanged.
- `vignette` brightens or darkens the corners after local masks.
- `dehaze` removes airlight with the dark channel prior.
- `highlights`, `shadows` and `clarity` work on a base/detail split of log
  luminance. The base layer is an edge-aware (guided) smoothing computed at
  reduced size and applied at full resolution, so large regions can be
  brightened or darkened without halos and without flattening texture.
- A filmic sigmoid maps scene luminance to display range. Middle grey stays
  at 0.18, black at zero, and both ends roll off smoothly. The zero-slider
  exponent is 1.8, giving about 1.49 log-log contrast at middle grey with the
  native white point (formerly 1.33 at exponent 1.6). Its toe deepens low
  shadows continuously without a fixed black offset. An upper Hermite
  shoulder joins the sigmoid halfway to its scene white point and reaches
  white with zero derivative.
  One RGB gain transports the mapped luminance, preserving chromaticity
  and Oklab C/L, including signed matrix channels. The former blend towards per-channel highlight curves desaturated coloured
  highlights and rotated their hue. Colour now loses chroma only when the
  final sRGB gamut requires it, at constant Oklab lightness and hue. The subsequent contrast-linked colour rendering below adds
  bounded colour separation. There is no fixed saturation/vibrance boost or
  camera-style colour table.
  `contrast` changes the slope; `whites` adjusts the display white point.
- `blacks` and the optional point `curve` act on display values. Light
  above display white passes through them unclipped (the curve continues
  with slope one above 1) and reaches gamut mapping as it does without
  them; clipping each channel there drew a hard-edged disc around low suns.

## 8. Colour rendering and adjustments — `color.rs`, `adjust.rs`

Default colour rendering accompanies the filmic step, before display
white/black points and the point curve. Photographic contrast also separates
colours. Pure luminance gain would
remove that separation from shadows and midtones; the old per-channel
curve supplied it there but washed out shoulder colours. Default colour
rendering carries half the local tone contrast in log space: at scene
luminance Y, chroma gain is `sqrt(max(1, Y F'(Y) / F(Y)))`. Oklab supplies
hue-preserving chroma separation. This conservative policy varies with
scene level and the contrast curve, rather than applying a fixed gain to
all colours. The gain fades smoothly for existing C/L from 0.1 to 0.3;
already vivid colours get no extra separation. Where the shoulder's slope
is at most one there is no expansion or desaturation. After chroma changes,
a common RGB normalization restores mapped luminance; neutral colours get
no additive tint. Saturation ramps remain monotone throughout the slider
range. Gamut compression follows at output.

`vibrance`, `saturation`, the eight `hsl` bands and the two-way colour
`grade` operate in Oklab, where hue, chroma and lightness can be changed
independently.

## 9. Output — `output.rs`

Colours outside sRGB are brought inside by reducing chroma at constant
lightness and hue. The result is encoded as sRGB, sharpened on encoded
luminance with an unsharp mask limited to the local range (edges steepen,
they do not halo), then clamped.

Only after these native-resolution stages is the finished sRGB image
area-averaged to `--size`. Alternatively, `--region` extracts native-resolution
detail from the finished frame without changing filter support; the CLI
rejects combining it with `--size`. Output pixels are finally dithered
to 8 bits for JPEG or written as 16-bit PNG. Output files carry no EXIF,
GPS or serial numbers. The resize gives the same noise, sharpness and tone
as resizing a full render with the same area
filter, rather than changing spatial radii or processing averaged linear
pixels. Previews therefore do full-resolution work and use full-resolution
working memory. `compare` uses this path for developed panels too. Scalar
filter planes resize without RGB expansion; box filters retain their exact
running-sum order while processing neighbouring columns together and
reusing disposable buffers. Chroma channels share guide statistics, and
native rendering finishes its owned scene without copying it.
[Stage diagnostics and measured before/after results](performance.md).

`examples/quality.rs --check` compares a noisy synthetic RAW at several sizes
against the downscaled full render, with noise standard deviation, edge
acutance, mean, p1/p50/p95/p99 and maximum channel error. Its tolerances are
1% relative for noise/acutance and 0.001 in encoded 0–1 units for mean,
percentiles and channel error; the current path is exactly equal before
output quantisation. This guarantee is relative to the same area resize,
not an arbitrary viewer filter or separately compressed JPEG. PNG rounding
and JPEG quantisation/compression can introduce additional differences.
The high-ISO-like synthetic scene (square-root noise 0.014, as measured at
ISO 6400) separately checks both low-frequency chroma axes (at least 50% RMS
removal) and, at the strength matching its noise, flat-area luminance noise
(at least 70% removed) and the projection of a fine texture four noise
levels high (at least 90% retained). Strengths 85 and 100 are reported as
well: they remove more and keep less. These are deterministic synthetic
floors, not claims about every
real camera or photo. Fine grain and very large colour clouds may remain.

`examples/quality.rs` also checks the default middle-grey log slope
(1.45–1.55), toe at scene 0.01 (below 0.002), hue and relative-saturation
transport through the luminance curve, contrast-linked colour rendering and
its finished in-gamut output, and a noisy ISO-6400-like RAW's measured default. The noisy default
must match rendering with its explicit resolved amount, exceed a quiet
same-ISO field by at least 20 slider points and remove at least 50% chroma
noise. These synthetic checks do not establish a camera-JPEG preference.

Statistics describe the resulting output size: full-resolution statistics
can legitimately differ from statistics of its resized version.

## Recipe versions

Versions 0.2.0 and 0.4.0 change pixels for existing recipes: 0.2.0 through
the default tone and colour, 0.4.0 wherever `denoise` resolves above 10
(luminance grain is now removed) or `defringe` is above zero (coloured
specks are calmed too). Recipes that develop a low-ISO photo without
defringe render the same bytes in 0.3.0 and 0.4.0. The `rawsmith`
writer version is provenance, not a pinned pipeline: omitted fields adopt
current defaults and explicit settings are retained. Keeping an obsolete
pipeline would retain its colour and noise shortcomings and double the
rendering contracts. `recipe_compatibility` reports the writer and rendering
versions, the `current_defaults` policy and a note for different or absent
writer versions; `compare` reports it per developed panel. Rendering and
comparison never rewrite recipes. To reproduce the original look, use the
original executable with the original RAW and recipe.

## Automatic starting point — `auto.rs`

`rawsmith auto` keeps white balance, lens settings, framing, sharpening and
non-zero additional defringe strength and explicit denoise amount from its
base recipe, and replaces tone and colour values, clearing manual gradients,
masks and vignette. Unset denoise uses
the same measured default policy as rendering. Tone and colour candidate
measurements run through the current default curve and gamut mapping, so
old tone/colour settings do not stack on the new base.
When more than 0.5% of framed native tile samples border sensor loss
(any photosite at 98%) and a strong edge (neighbouring square-root green
difference at least 0.25), a zero base defringe becomes 50; the one-in-200
coverage gate rejects isolated loss, while the edge threshold separates
near-white green from neighbours below roughly 0.55 of sensor range,
including a one-pixel dark line covering one of a tile's two greens.
This is a heuristic fringe-risk proxy, not detection of purple objects,
and `decisions` reports its coverage, threshold and chosen strength.
It evaluates ordinary recipe settings on a 640 px scene. `decisions`
reports the evidence, confidence, bounds and settings, including decisions
to leave an adjustment at zero. There are no new recipe parameters.

The measurements and policy are:

- **Sensor loss:** a stratified sample of native 2×2 Bayer tiles measures
  black-subtracted saturation before white balance and reconstruction.
  A photosite at 98% of its range is counted as near saturation, the same
  safety margin as reconstruction. Any-site and all-four-site loss are
  reported separately. Tiles are ranked by their two green samples;
  the brightest fifth's loss weights partial loss by one half and complete
  loss by one. Partial loss can retain texture in surviving channels;
  complete loss cannot. The sampled map follows orientation, crop and
  rotation, but not lens resampling. These approximate decision measurements
  are distinct from the render report's full-mosaic `sensor_clipped_pct`.
- **Noise (shared with defaults):** up to roughly 64×64 small native patches are sampled before
  resizing or denoising. Same-colour second differences against four
  neighbours remove smooth gradients. RMS in square-root sensor encoding
  is divided by the stencil's noise amplification (sqrt(1.25)); the quietest
  quarter of patch estimates rejects much texture. The estimate sets an
  amount relative to 0.02, the denoiser's maximum square-root regularisation
  scale. The proposal combines 65% measured amount with 35% ISO prior
  (`10 + 10 log2(ISO/100)`, capped at 85), bounded to 10–90 and rounded to
  five. The amount is also capped at the ISO prior +20 because uniformly
  fine texture can resemble noise. Fewer than 16 usable patches falls back to the prior. This is a
  noise proxy, not calibrated sensor characterisation: textured frames,
  very dark patches, white-balance/channel gains and lens shading can bias
  it. It does not measure slow colour mottling. Review detail at 100%.
- **High-key evidence:** a broad bright-neutral area (Oklab L > 55,
  C < 3, both on a 0–100 scale) must coexist with a meaningful p90–p10
  range. Approximate completely sensor-lost coverage is subtracted from
  bright-neutral coverage: a blown white background cannot establish a
  diffuse white subject. Confidence rises over 35–65% usable coverage and a
  12–24 point range, and fades for chromatic highlights (C 3–6).
  Low mean chroma alone cannot identify snow or fog: grey water, walls
  and clear shaded snow are ambiguous. The classifier describes a tonal
  distribution, not semantic recognition of a subject. Confidence values
  are heuristic gate strengths, not calibrated probabilities.
- **Exposure and highlight cost:** local shadow/highlight adjustments precede
  exposure. Shadow lifting also uses backlight confidence and a dark median,
  and is cut by up to 65% for night scenes (EV100 from aperture, shutter and
  ISO, fading in from 7 down to 3), which are meant to stay dark,
  rather than just p10, to open a shaded subject (up to +50). Small bright
  regions cannot justify negative global EV. Only a broadly bright median
  (L50 > 85), weak high-key evidence (< 0.35) and < 1% complete sensor loss
  permit negative EV, bounded at −0.3; this reason is reported. The default
  median floor is one point below the metered rendering, raised by up to four
  points for backlight. The ordinary median ceiling is two points above the
  default, with additional room for high-key and backlit scenes. Exposure
  searches up to +0.5 EV, or +1.2 with strong high-key/camera brightness
  evidence. Complete sensor loss occupying the median holds exposure at zero.
  Seven bisections find the brightness floor and seven search for exposure;
  values stay on a 0.05 EV grid within the ceiling.
  Highlight limits are L90 96 and L99 99, relaxed by backlit sensor loss;
  complete loss occupying those percentiles removes their limits (10% / 1%
  coverage). Output channels pinned at an sRGB gamut boundary below L90 are
  not evidence of highlight overexposure. Bright-channel clipping (L > 90,
  any encoded channel >= 0.998) is compared with sampled any-site sensor
  loss +0.2 percentage points; pure-white clipping growth is limited to 0.2.
  Local highlight trials require improved highlight measurements without
  violating brightness floors. Suppression is capped at −30, weakened
  towards zero by weighted sensor loss in the brightest fifth. Unresolved
  targets are reported instead of burying the foreground or endlessly
  flattening irrecoverable sky.
- **Camera tone sanity bound:** the library entry `auto_with_file` accepts
  original RAW bytes and reuses `compare`'s verified sRGB/Adobe RGB conversion.
  Median and upper-midtone L75 are lower bounds with a two-point tolerance,
  after orientation and recipe framing; L75 is skipped when complete sensor
  loss covers at least 25%. L75−L50 supplies an additional contrast floor,
  tried up to +30 contrast. This constrains brightness/flatness without copying
  camera colour, white balance or local rendering. Decisions identify the
  measurements, raised bounds and any unresolved limit. Unattainable floors
  fall back to attainable brightness at the exposure ceiling with one point
  of room for finishing; the original unmet bound is still reported.
  Missing, corrupt or unverified JPEG colour uses the finished fallback with
  a reason. The existing `auto(raw, base)` entry has no original bytes and
  reports that the camera bound is unavailable; CLI integration must supply
  those bytes to `auto_with_file` to enable it.
- **Haze evidence:** an 8×8 grid must contain raised dark channels with
  muted colour and subdued but nonzero texture, alongside blocks with
  firmer contrast. Per-block p90–p10 ranges of 2–20 lightness points,
  dark-channel p10 of encoded 0.18–0.35, C/L of 0.025–0.18 and median
  below 85 form soft evidence. Veiled coverage must exceed 10% and
  textured coverage 5%; full confidence requires 35% / 20%. High-key
  confidence suppresses dehaze. Uniform grey, smooth coloured sky and
  a raised global black level alone cannot justify it. With evidence,
  dehaze is capped at 15; atmospheric depth can intentionally remain soft.
- **Contrast and points:** uncertain or flat distributions get +15 to +20
  contrast, fading only for established broad contrast (spread 22–32) and
  reduced by up to half for high-key evidence. Black deepening remains capped
  at −5 and suppressed for backlight/high-key subjects. High-key whites are
  capped at +30 and scaled by squared confidence; trials reduce them in steps
  of five if L99 exceeds 96 or pure-white clipping grows by over 0.2 points.
- **Final tone headroom:** dehaze, contrast, blacks and whites are trialled
  separately. A trial may compensate exposure within its existing ceiling
  to preserve median/L75 floors, rather than discarding contrast merely
  because it darkened a low median. Each trial keeps bright-channel and
  pure-white clipping growth within 0.2 percentage points of the initial
  exposure/local-highlight rendering. Failed controls back off in steps of
  five. Camera contrast trials use the same guards and report unmet bounds.
- **Colour:** p90 relative Oklab chroma C/L, excluding L <= 20, detects vivid
  subjects among neutral surroundings. A +20 vibrance budget fades from
  C/L 0.14 to 0.30; nearly achromatic frames receive no boost (mean chroma
  ramp 0.5–1.5). A small near-clipped region no longer vetoes colour for the
  whole picture. Trials reduce vibrance in steps of five if brightness
  floors fail or bright-channel clipping grows by more than 0.2 percentage
  points. Already-vivid subjects retain zero boost. Clarity remains zero:
  global statistics cannot separate helpful texture from skin, noise or blur.

These are bounded finishing defaults with measured exceptions, not individual
photo recipes. Weak evidence falls back to a finished look; strong evidence
can reduce an adjustment. White balance is reported, never guessed from blue
shade or warm light. Every guard and unresolved bound appears in decisions.

The additional sampling and candidate work affects **auto analysis only**.
Recipes without local masks render through the existing pipeline without new per-pixel
stages. Exposure compensation and bounded camera/highlight/colour trials add
small-image work; performance must be measured on the designated host, not
inferred from these correctness runs.

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
