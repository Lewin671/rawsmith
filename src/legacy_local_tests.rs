//! Portable snapshot of the pre-mask finish path, including legacy gradient arithmetic.
use super::*;

fn original_finish(mut image: Image, params: &Params, spaces: &Spaces) -> Image {
    let stage = crate::profile::Stage::new("exposure/local tone");
    let base = BASE_EXPOSURE;
    let gain = (params.exposure as f32 + base).exp2();
    image.map(|p| [p[0] * gain, p[1] * gain, p[2] * gain]);

    if !params.gradients.is_empty() {
        let (w, h) = ((image.width.max(2) - 1) as f32, (image.height.max(2) - 1) as f32);
        let gradients: Vec<_> = params
            .gradients
            .iter()
            .map(|g| {
                let from = [g.from[0] as f32 * w, g.from[1] as f32 * h];
                let axis = [g.to[0] as f32 * w - from[0], g.to[1] as f32 * h - from[1]];
                let scale = 1.0 / (axis[0] * axis[0] + axis[1] * axis[1]).max(1e-6);
                (from, [axis[0] * scale, axis[1] * scale], g.exposure as f32, g.saturation as f32 / 100.0)
            })
            .collect();
        image.map_xy(|x, y, mut p| {
            for (from, axis, exposure, saturation) in &gradients {
                // Position along the gradient: 0 at `from`, 1 at `to`.
                let t = ((x as f32 - from[0]) * axis[0] + (y as f32 - from[1]) * axis[1]).clamp(0.0, 1.0);
                let weight = 1.0 - t * t * (3.0 - 2.0 * t);
                let gain = (exposure * weight).exp2();
                let grey = color::luma(p);
                let chroma = (1.0 + saturation * weight).max(0.0);
                p = p.map(|v| (grey + (v - grey) * chroma) * gain);
            }
            p
        });
    }
    if params.vignette != 0.0 {
        // Up to one stop at the corners, growing with the square of the radius.
        let (cx, cy) = ((image.width as f32 - 1.0) * 0.5, (image.height as f32 - 1.0) * 0.5);
        let (stops, norm) = (params.vignette as f32 / 100.0, 1.0 / (cx * cx + cy * cy).max(1.0));
        image.map_xy(|x, y, p| {
            let r2 = ((x as f32 - cx).powi(2) + (y as f32 - cy).powi(2)) * norm;
            p.map(|v| v * (stops * r2).exp2())
        });
    }
    tone::dehaze(&mut image, params.dehaze as f32 / 100.0);
    tone::local_tone(
        &mut image,
        &Local {
            highlights: params.highlights as f32 / 100.0,
            shadows: params.shadows as f32 / 100.0,
            clarity: params.clarity as f32 / 100.0,
        },
    );

    drop(stage);
    let stage = crate::profile::Stage::new("filmic/colour/gamut");
    // Sensor saturation sits at 2^BASE_EXPOSURE and just reaches display white.
    let filmic = Filmic::new(params.contrast as f32, base.exp2());
    // Black and white points on encoded display values: `whites` moves the
    // value that maps to pure white, `blacks` the one that maps to black.
    let black = -params.blacks as f32 / 100.0 * 0.08;
    let white_point = 1.0 - params.whites as f32 / 100.0 * 0.25;
    let curve = (!params.curve.is_empty()).then(|| tone::curve_lut(&params.curve, 1025));
    let ops = ColorOps::new(params, spaces);
    image.map(|p| {
        let mut p = spaces.render_colour(filmic.map_rgb(p), filmic.log_slope(color::luma(p)));
        if black != 0.0 || white_point != 1.0 || curve.is_some() {
            p = p.map(|v| {
                let mut e = srgb_encode(v);
                e = ((e - black) / (white_point - black)).clamp(0.0, 1.0);
                if let Some(lut) = &curve {
                    e = tone::lut_eval(lut, e);
                }
                color::srgb_decode(e)
            });
        }
        p
    });
    ops.run(&mut image, spaces);
    image.map(|p| spaces.to_srgb_gamut(p).map(srgb_encode));
    drop(stage);
    let stage = crate::profile::Stage::new("sharpen/clamp");
    sharpen(&mut image, params.sharpen as f32 / 100.0 * 1.5);
    image.map(|p| p.map(|v| v.clamp(0.0, 1.0)));
    drop(stage);
    image
}

#[test]
fn legacy_finish_pixels_and_encoded_bytes_are_unchanged() {
    let spaces = Spaces::new();
    for (w, h) in [(1, 1), (197, 83), (64, 129)] {
        let image = Image {
            width: w,
            height: h,
            data: (0..w * h)
                .map(|i| {
                    let v = (i % 181) as f32 / 180.0;
                    [v * 0.65, v * 0.9, v * 0.7 - 0.01]
                })
                .collect(),
        };
        for text in [
            "{}",
            r#"{"exposure":0.2,"contrast":17,"shadows":24,"highlights":-30,"vibrance":15,"vignette":-15,"curve":[[0.2,0.22]],"gradients":[{"from":[0.1,0.2],"to":[0.87,0.91],"exposure":-0.7,"saturation":27},{"from":[0.9,0.7],"to":[0.05,0.2],"exposure":0.3,"saturation":-31}]}"#,
        ] {
            let params = Params::from_json(text).unwrap();
            let original = original_finish(image.clone(), &params, &spaces);
            let current = finish_owned(image.clone(), &params, &spaces);
            assert_eq!(original.data, current.data, "{w}x{h} {text}");
            for path in ["legacy.png", "legacy.jpg"] {
                assert_eq!(
                    crate::output::encode(std::path::Path::new(path), &original, 92).unwrap(),
                    crate::output::encode(std::path::Path::new(path), &current, 92).unwrap()
                );
            }
        }
    }
}
