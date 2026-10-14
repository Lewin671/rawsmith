//! Default rendering contracts, independent of any camera JPEG style.
use rawsmith::{
    arw, auto,
    color::{self, Spaces},
    image::Image,
    noise,
    params::Params,
    pipeline::{self, Size},
    synthetic::Synthetic,
    tone::{Filmic, GREY},
};

#[test]
fn shoulder_keeps_colour_hue_and_saturation_until_output_gamut() {
    let spaces = Spaces::new();
    let filmic = Filmic::new(0.0, pipeline::BASE_EXPOSURE.exp2());
    for hue in (0..360).step_by(15) {
        for level in [0.002f32, 0.05, GREY, 0.5, 1.2, 2.0] {
            for saturation in [0.04, 0.15, 0.35] {
                let angle = (hue as f32).to_radians();
                let l = level.cbrt();
                // Strong colours exercise signed camera-matrix channels too.
                let input_lab = [l, l * saturation * angle.cos(), l * saturation * angle.sin()];
                let input = spaces.from_oklab(input_lab);
                let mapped = filmic.map_rgb(input);
                let lab = spaces.to_oklab(mapped);
                assert!((lab[1].hypot(lab[2]) / lab[0] - saturation).abs() < 0.0001);
                let direction_error = (lab[1] * angle.sin() - lab[2] * angle.cos()).abs();
                assert!(direction_error < 0.00001, "hue {hue}, level {level}");
                let out = spaces.to_srgb_gamut(mapped);
                assert!(out.iter().all(|v| v.is_finite() && (0.0..=1.0).contains(v)));
                let out_lab = spaces.to_oklab(color::apply(&spaces.srgb_to_work, out));
                assert!(out_lab[1].hypot(out_lab[2]) <= lab[1].hypot(lab[2]) + 0.0001);
            }
        }
    }
    assert_eq!(filmic.map_rgb([0.0; 3]), [0.0; 3]);
    assert_eq!(filmic.map_rgb([-1.0; 3]), [0.0; 3]);
}

fn noisy_raw(amplitude: f32) -> arw::RawImage {
    let mut raw = arw::decode(
        &Synthetic::from_camera_rgb(192, 128, |x, y| {
            let hash = (x as u32).wrapping_mul(747796405).wrapping_add((y as u32).wrapping_mul(2891336453));
            let grain = ((hash ^ (hash >> 16)) % 1001) as f32 / 500.0 - 1.0;
            let level = 0.1 + amplitude * grain;
            [level / (2600.0 / 1024.0), level, level / (1600.0 / 1024.0)]
        })
        .to_bytes(),
    )
    .unwrap();
    raw.meta.iso = Some(6400);
    raw
}

#[test]
fn defaults_and_auto_share_noise_and_explicit_strengths_are_exact() {
    let raw = noisy_raw(0.025);
    let measured = noise::resolve(&raw, None);
    assert_eq!(measured.source, "measured");
    assert!(measured.strength >= 50.0);
    assert!(measured.strength > noise::resolve(&noisy_raw(0.0), None).strength + 20.0);
    let proposal = auto::auto(&raw, &Params::default()).unwrap();
    assert_eq!(proposal.params.denoise, None);
    assert_eq!(noise::resolve(&raw, proposal.params.denoise), measured);
    assert!(proposal.notes.iter().any(|note| note.starts_with(&format!("denoise {:.0}:", measured.strength))));
    for amount in [0.0, 10.0, 52.44140625, 100.0] {
        let params = Params { denoise: Some(amount), ..Params::default() };
        assert_eq!(auto::auto(&raw, &params).unwrap().params.denoise, Some(amount));
        let scene = pipeline::scene(&raw, &params, Size::default()).unwrap();
        assert_eq!(scene.noise_reduction.strength, amount);
        assert_eq!(scene.noise_reduction.source, "recipe");
        assert_eq!(scene.noise_reduction.native_sqrt_noise, None);
        assert_eq!(Params::from_value(params.to_sparse()).unwrap().denoise, Some(amount));
    }
    let params = Params::from_json(r#"{"crop":{"top":0.4},"temperature":5200}"#).unwrap();
    for edge in [None, Some(1), Some(64)] {
        let rendered = pipeline::render(&raw, &params, Size { max_edge: edge }).unwrap();
        assert_eq!(rendered.noise_reduction, measured);
    }
}

#[test]
fn insufficient_patches_fall_back_to_iso_prior_and_null_resets_measurement() {
    let mut raw = arw::decode(&Synthetic::from_camera_rgb(16, 16, |_, _| [0.1; 3]).to_bytes()).unwrap();
    raw.meta.iso = Some(6400);
    let fallback = noise::resolve(&raw, None);
    assert_eq!((fallback.strength, fallback.source, fallback.native_sqrt_noise), (80.0, "iso_prior", None)); // prior 70 plus the speck margin
    let p = Params::from_json(r#"{"denoise":10}"#).unwrap();
    assert_eq!(p.to_sparse()["denoise"], 10.0);
    let reset = p.with_assignments(&["denoise=null".into()]).unwrap();
    assert_eq!(reset.denoise, None);
    assert!(reset.to_sparse().get("denoise").is_none());
    assert!(reset.with_assignments(&["denoise+=5".into()]).is_err());
    for json in [r#"{"denoise":-1}"#, r#"{"denoise":101}"#, r#"{"denoise":true}"#] {
        assert!(Params::from_json(json).is_err());
    }
}

#[test]
fn auto_replaces_old_tone_and_colour_instead_of_stacking_on_the_new_base() {
    let raw = noisy_raw(0.0);
    let baseline = auto::auto(&raw, &Params::default()).unwrap();
    let old_edit = Params { contrast: 80.0, vibrance: 90.0, exposure: 2.0, shadows: 60.0, ..Params::default() };
    let got = auto::auto(&raw, &old_edit).unwrap();
    assert_eq!(got.params, baseline.params);
    assert_eq!(got.notes, baseline.notes);
}

#[test]
fn grey_output_has_one_srgb_transfer_and_no_default_colour_boost() {
    let scene = Image { width: 1, height: 1, data: vec![[GREY / pipeline::BASE_EXPOSURE.exp2(); 3]] };
    let out = pipeline::finish(&scene, &Params::default(), &Spaces::new());
    for value in out.data[0] {
        assert!((value - color::srgb_encode(GREY)).abs() < 0.0001, "{value} expected {}", color::srgb_encode(GREY));
    }
}

#[test]
fn contrast_linked_colour_protects_vivid_saturation_and_keeps_the_tone_curve() {
    let spaces = Spaces::new();
    for contrast in [-100.0, 0.0, 100.0] {
        let filmic = Filmic::new(contrast, pipeline::BASE_EXPOSURE.exp2());
        for level in [0.003f32, 0.05, GREY, 0.6, 1.5, 2.2] {
            for hue in [29.0f32, 58.0, 105.0, 258.0, 300.0] {
                let angle = hue.to_radians();
                let mut previous = 0.0;
                for i in 0..401 {
                    let saturation = i as f32 / 1000.0;
                    let l = level.cbrt();
                    let input = spaces.from_oklab([l, l * saturation * angle.cos(), l * saturation * angle.sin()]);
                    let mapped = filmic.map_rgb(input);
                    let log_slope = filmic.log_slope(color::luma(input));
                    let rendered = spaces.render_colour(mapped, log_slope);
                    let lab = spaces.to_oklab(rendered);
                    let result = lab[1].hypot(lab[2]) / lab[0];
                    assert!(result >= previous - 0.00001, "saturation inversion at {contrast}, {level}, {hue}, {i}");
                    assert!(
                        result >= saturation - 0.00001 && result <= saturation * log_slope.max(1.0).sqrt() + 0.00001
                    );
                    assert!((color::luma(rendered) - color::luma(mapped)).abs() < 0.00001);
                    if saturation >= 0.3 || log_slope <= 1.0 {
                        assert!((result - saturation).abs() < 0.00001);
                    }
                    assert!((lab[1] * angle.sin() - lab[2] * angle.cos()).abs() < 0.00001);
                    let output = spaces.to_srgb_gamut(rendered);
                    assert!(output.iter().all(|v| v.is_finite() && (0.0..=1.0).contains(v)));
                    previous = result;
                }
            }
        }
    }
    // At middle grey, small colour differences carry half the tone contrast
    // in log space. This is a local Jacobian property, not a fixed boost.
    let f = Filmic::new(0.0, pipeline::BASE_EXPOSURE.exp2());
    let h = GREY * 0.001;
    let measured_slope = GREY * (f.eval(GREY + h) - f.eval(GREY - h)) / (2.0 * h * f.eval(GREY));
    let muted = spaces.from_oklab([GREY.cbrt(), 0.02 * GREY.cbrt(), 0.0]);
    let muted = muted.map(|v| v * GREY / color::luma(muted));
    let result = spaces.to_oklab(spaces.render_colour(f.map_rgb(muted), f.log_slope(color::luma(muted))));
    assert!((result[1].hypot(result[2]) / result[0] / 0.02 - measured_slope.sqrt()).abs() < 0.001);
    // Skin-like and shadow tints keep hue, with bounded chroma expansion.
    for level in [0.005f32, 0.18, 0.5] {
        let l = level.cbrt();
        let skin = spaces.from_oklab([l, 0.05 * l, 0.08 * l]);
        let mapped = f.map_rgb(skin);
        let output = spaces.to_oklab(spaces.render_colour(mapped, f.log_slope(color::luma(skin))));
        assert!(output[1].hypot(output[2]) / output[0] <= 0.095 * 1.35);
    }
}

#[test]
fn analytic_local_contrast_matches_the_curve_including_its_shoulder() {
    for contrast in [-100.0, 0.0, 100.0] {
        for white in [1.0, pipeline::BASE_EXPOSURE.exp2(), 16.0] {
            let f = Filmic::new(contrast, white);
            for x in [0.003, GREY, white * 0.49, white * 0.51, white * 0.75, white * 0.95] {
                let h = x * 0.001;
                let measured = x * (f.eval(x + h) - f.eval(x - h)) / (2.0 * h * f.eval(x));
                assert!((measured - f.log_slope(x)).abs() < 0.002, "contrast {contrast}, white {white}, x {x}");
            }
            assert_eq!(f.log_slope(white), 0.0);
            assert_eq!(f.log_slope(white * 2.0), 0.0);
        }
    }
}
