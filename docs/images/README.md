# Documentation photographs

- `demo-camera.jpg` and `demo-rawsmith.jpg`: a CC0 Sony A7 IV sample
  from [raw.pixls.us](https://raw.pixls.us), as described in the main README.
- `snowman-comparison.jpg`, `sunset-comparison.jpg` and
  `snow-mountain-comparison.jpg`: user-provided Sony A7 IV photographs,
  reproduced with permission for this repository's documentation. These
  photographs are not CC0 and are not covered by the code's MIT/Apache-2.0
  licences; no permission for other reuse is granted here.

Comparison images are resized JPEGs without EXIF metadata. No source RAW
files are included. The left image is the camera's embedded JPEG; the
right image is developed with rawsmith using the recipes in
[`../recipes`](../recipes). Both panels use the recipe's crop and rotation.
The three private-photo comparisons were regenerated after discovering that
their previous camera panels displayed Adobe RGB values unconverted as sRGB,
which made the camera rendering look duller and less saturated. `compare`
now converts those camera JPEGs to sRGB and tags the combined JPEG.
No text is drawn on the images; the README caption is
"Camera JPEG | edited with rawsmith".

Exact commands, run from the repository root with the three authorized RAWs
in `$RAWS` (the owner's private source directory, not included here):

```sh
target/release/rawsmith compare "$RAWS/_DSC3169.ARW" --panel camera --panel recipe:docs/recipes/sunset.json -o docs/images/sunset-comparison.jpg --size 1000
target/release/rawsmith compare "$RAWS/_DSC3175.ARW" --panel camera --panel recipe:docs/recipes/snowman.json -o docs/images/snowman-comparison.jpg --size 1000
target/release/rawsmith compare "$RAWS/_DSC3284.ARW" --panel camera --panel recipe:docs/recipes/snow-mountain.json -o docs/images/snow-mountain-comparison.jpg --size 1000
```

Each panel's longest edge is at most 1000 px; the row has a two-pixel grey
separator. `compare` encodes at JPEG quality 92 with an sRGB ICC profile and
no EXIF; no separate metadata-stripping step was applied.

## One-command auto example (CC0 sample)

| Camera JPEG | `rawsmith auto` |
|---|---|
| ![The camera's own rendering](demo-camera.jpg) | ![rawsmith auto rendering](demo-rawsmith.jpg) |

A CC0 sample from [raw.pixls.us](https://raw.pixls.us) (Sony A7 IV), developed with one command.
