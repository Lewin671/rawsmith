# Changelog

Recipes store only what differs from the defaults, so a release that
changes the defaults changes how older recipes render; render reports say
so in `recipe_compatibility`.

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
