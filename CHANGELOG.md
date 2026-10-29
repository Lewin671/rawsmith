# Changelog

Recipes store only what differs from the defaults, so a release that
changes the defaults changes how older recipes render; render reports say
so in `recipe_compatibility`.

## 0.7.0

- Crops keep the photo's aspect ratio by default. Edges that ask for
  another shape are narrowed to the largest rectangle of the original
  ratio, centred in the requested box, so `crop.top=0.1` alone also trims
  5% from each side. `crop.aspect` takes a ratio such as `4:5` or `1:1`,
  or `free` for the edges exactly as given.
- Render, edit and auto reports give the applied crop edges in `crop`.
- Older recipes whose crop changed the shape now render with the original
  ratio; add `"aspect": "free"` to their `crop` to keep the old framing.

## 0.6.1

- Removes pale cyan and pink tints from twigs and leaf edges against a
  blown sky.
- Keeps thin trunks and branches through high-ISO noise reduction.
- A `denoise` set below the measured strength now leaves even fine grain
  instead of specks and blotches on fog and water.
- Recipes made with 0.6.0 can render differently where these fixes apply.

## 0.6.0

Came out of rounds of AI editing on real photos, fixing what the editors
tripped on:

- `compare --region` puts the camera's pixels beside yours at full
  resolution.
- `compare` notes colours that turned to another hue.
- Black point and curve no longer draw a hard disc around the sun.
- High-ISO noise reduction clears specks on smooth water and fog.
- `auto` keeps night scenes dark and defringes clipped twigs harder.
- The skill teaches readable shadows, decisive crops and a dozen recurring
  pitfalls.

Rendering of existing recipes changes only where those fixes apply
([acceptance](docs/benchmark.md#060-editing-acceptance)).

## Earlier releases

See [GitHub releases](https://github.com/Lewin671/rawsmith/releases).
