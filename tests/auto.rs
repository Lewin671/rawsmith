//! Automatic decisions on scenes with known causes, independent of photo IDs.
use rawsmith::{arw, auto, params::Params, synthetic::Synthetic};

fn propose(pixel: impl Fn(usize, usize) -> [f32; 3]) -> auto::Auto {
    let raw = arw::decode(&Synthetic::from_camera_rgb(128, 96, pixel).to_bytes()).unwrap();
    auto::auto(&raw, &Params::default()).unwrap()
}

fn grey(level: f32) -> [f32; 3] {
    [level / (2600.0 / 1024.0), level, level / (1600.0 / 1024.0)]
}

#[test]
fn vivid_subject_in_neutral_frame_does_not_get_colour_boost() {
    let result =
        propose(|x, y| if x < 32 && y < 48 { [0.15, 0.02, 0.12] } else { grey(0.10 + 0.03 * y as f32 / 96.0) });
    println!("vivid: {:?}", result.params.to_sparse());
    assert_eq!(result.params.vibrance, 0.0, "a vivid subject must not be hidden by the frame mean");
}

#[test]
fn sensor_saturated_sky_does_not_bury_foreground() {
    let result = propose(|_, y| if y < 24 { [1.0; 3] } else { grey(0.025 + 0.015 * y as f32 / 96.0) });
    println!("backlight: {:?}", result.params.to_sparse());
    assert!(result.params.exposure >= -0.3, "darkening cannot restore the all-channel saturated sky");
    assert!(result.params.blacks >= -5.0, "do not add crushed blacks to backlight");
}

#[test]
fn flat_grey_water_is_not_snow_or_haze() {
    let result = propose(|x, y| grey(0.10 + 0.003 * ((x + y) % 16) as f32 / 16.0));
    println!("grey: {:?}", result.params.to_sparse());
    assert!(result.params.exposure <= 0.2, "flat middle grey has no evidence for a high-key lift");
    assert_eq!(result.params.whites, 0.0);
    assert_eq!(result.params.dehaze, 0.0);
    assert_eq!(result.params.clarity, 0.0);
}

#[test]
fn clear_snow_with_blue_sky_is_not_haze() {
    let result =
        propose(|x, y| if y < 32 { [0.025, 0.12, 0.32] } else { grey(0.25 + 0.2 * ((x / 8 + y / 8) % 2) as f32) });
    println!("snow: {:?}", result.params.to_sparse());
    assert_eq!(result.params.dehaze, 0.0, "snow and a coloured sky are not an atmospheric veil");
    assert!(result.params.exposure <= 0.5, "bright snow needs tonal room");
}

#[test]
fn equal_iso_does_not_mean_equal_measured_noise() {
    let make = |amplitude: f32| {
        let mut raw = arw::decode(
            &Synthetic::from_camera_rgb(192, 128, |x, y| {
                let hash = (x as u32).wrapping_mul(747796405).wrapping_add((y as u32).wrapping_mul(2891336453));
                let noise = ((hash ^ (hash >> 16)) % 1001) as f32 / 500.0 - 1.0;
                grey(0.10 + amplitude * noise)
            })
            .to_bytes(),
        )
        .unwrap();
        raw.meta.iso = Some(6400);
        let proposal = auto::auto(&raw, &Params::default()).unwrap();
        rawsmith::noise::resolve(&raw, proposal.params.denoise).strength
    };
    let clean = make(0.0);
    let noisy = make(0.025);
    println!("same ISO denoise: clean {}, noisy {}", clean, noisy);
    assert!(noisy > clean + 20.0);
}

#[test]
fn diffuse_bright_neutrals_with_shadows_can_be_lifted() {
    let result =
        propose(|x, y| grey(if x < 24 { 0.015 + 0.01 * y as f32 / 96.0 } else { 0.10 + 0.025 * y as f32 / 96.0 }));
    println!("diffuse high key: {:?}", result.params.to_sparse());
    assert!(result.params.exposure > 0.3, "do not regress diffuse underexposed snow");
    assert!(result.params.exposure <= 1.2);
}

#[test]
fn spatial_atmospheric_veil_gets_moderate_dehaze() {
    let result = propose(|x, y| {
        let texture = if (x / 4 + y / 4) % 2 == 0 { -1.0 } else { 1.0 };
        if y < 64 {
            let level = 0.12 + texture * 0.015;
            // A modest blue veil, with subdued but real distant texture.
            [level * 0.70 / (2600.0 / 1024.0), level, level * 1.15 / (1600.0 / 1024.0)]
        } else {
            grey(0.055 + texture * 0.045)
        }
    });
    println!("haze: {:?}; {:?}", result.params.to_sparse(), result.notes);
    assert!(result.params.dehaze > 0.0 && result.params.dehaze <= 15.0);
}

#[test]
fn recoverable_coloured_highlights_do_not_require_dark_foreground() {
    use rawsmith::{
        color::Spaces,
        pipeline::{self, Size},
        stats,
    };
    let raw = arw::decode(
        &Synthetic::from_camera_rgb(128, 96, |x, y| {
            if y < 32 {
                [0.30 + 0.025 * x as f32 / 128.0, 0.45, 0.23]
            } else {
                grey(0.07)
            }
        })
        .to_bytes(),
    )
    .unwrap();
    let proposal = auto::auto(&raw, &Params::default()).unwrap();
    let scene = pipeline::scene(&raw, &proposal.params, Size { max_edge: Some(128) }).unwrap();
    let spaces = Spaces::new();
    let rendered = pipeline::finish(&scene.image, &proposal.params, &spaces);
    let report = stats::image_stats(&rendered, &spaces);
    let baseline = stats::image_stats(&pipeline::finish(&scene.image, &Params::default(), &spaces), &spaces);
    println!("coloured highlight: {:?}; {:?}", proposal.params.to_sparse(), report);
    assert!(proposal.params.exposure >= -0.3);
    assert!(proposal.params.highlights < 0.0);
    let control =
        Params { dehaze: 0.0, contrast: 0.0, whites: 0.0, vibrance: 0.0, sharpen: 0.0, ..proposal.params.clone() };
    let control = stats::image_stats(&pipeline::finish(&scene.image, &control, &spaces), &spaces);
    let finished = stats::image_stats(
        &pipeline::finish(&scene.image, &Params { sharpen: 0.0, ..proposal.params.clone() }, &spaces),
        &spaces,
    );
    assert!(
        finished.channel_clipped_pct <= control.channel_clipped_pct + 0.1,
        "later tone and colour additions must not undo established channel headroom"
    );
    assert!(baseline.channel_clipped_pct > 20.0, "the control must actually clip a colour channel");
    // Hue-preserving gamut mapping can pin a channel at the sRGB boundary
    // even after tone compression. Do not demand desaturation to lower this
    // count: test highlight lightness, foreground cost and disclosure instead.
    // Before 0.6 the black point clipped each channel at white, which
    // darkened this coloured highlight as a side effect (and drew hard
    // discs around low suns). Local compression alone moves it less.
    assert!(
        report.tones.p99 < baseline.tones.p99 - 0.5,
        "local compression should create highlight tonal room without global darkening"
    );
    assert!(
        proposal.notes.iter().any(|n| n.contains("target remains unresolved")),
        "residual clipping at conservative bounds must be disclosed"
    );
}

#[test]
fn smooth_coloured_gradient_alone_does_not_establish_haze() {
    let result = propose(|x, _| {
        let level = 0.18 + 0.035 * x as f32 / 128.0;
        [level * 0.42, level, level * 0.75]
    });
    assert_eq!(result.params.dehaze, 0.0);
}

#[test]
fn lost_white_background_is_not_evidence_for_a_high_key_lift() {
    let result = propose(|_, y| if y < 72 { [1.0; 3] } else { grey(0.025) });
    assert_eq!(
        result.params.exposure, 0.0,
        "a median occupied by sensor-lost whites cannot set exposure for the subject"
    );
    assert_eq!(result.params.whites, 0.0);
    assert!(result.notes.iter().any(|n| n.contains("occupies the median")));
}

fn rendered(raw: &arw::RawImage, params: &Params) -> rawsmith::stats::ImageStats {
    let spaces = rawsmith::color::Spaces::new();
    let scene = rawsmith::pipeline::scene(raw, params, rawsmith::pipeline::Size::default()).unwrap();
    rawsmith::stats::image_stats(&rawsmith::pipeline::finish(&scene.image, params, &spaces), &spaces)
}

#[test]
fn small_recoverable_highlight_does_not_darken_frame() {
    let raw = arw::decode(
        &Synthetic::from_camera_rgb(128, 96, |x, y| {
            if x < 8 && y < 32 {
                grey(0.95)
            } else {
                grey(0.07 + 0.03 * y as f32 / 96.0)
            }
        })
        .to_bytes(),
    )
    .unwrap();
    let result = auto::auto(&raw, &Params::default()).unwrap();
    let baseline = rendered(&raw, &Params::default());
    let finished = rendered(&raw, &result.params);
    assert!(result.params.exposure >= 0.0, "2% recoverable highlight must not set negative global EV");
    assert!(finished.tones.p50 >= baseline.tones.p50 - 1.0);
    result.params.validate().unwrap();
}

#[test]
fn muted_scene_gets_finished_contrast_and_colour() {
    let raw = arw::decode(
        &Synthetic::from_camera_rgb(128, 96, |x, y| {
            let level = 0.045 + 0.05 * y as f32 / 96.0;
            let tint = if x < 64 { 1.08 } else { 0.92 };
            [level * tint / (2600.0 / 1024.0), level, level / (1600.0 / 1024.0)]
        })
        .to_bytes(),
    )
    .unwrap();
    let result = auto::auto(&raw, &Params::default()).unwrap();
    let baseline = rendered(&raw, &Params::default());
    let finished = rendered(&raw, &result.params);
    println!("muted {:?}; {:?} -> {:?}", result.params.to_sparse(), baseline, finished);
    assert!(result.params.contrast >= 10.0);
    assert!(result.params.vibrance >= 10.0);
    assert!(finished.tones.p75 - finished.tones.p25 > baseline.tones.p75 - baseline.tones.p25 + 0.5);
    assert!(finished.chroma_mean > baseline.chroma_mean);
    assert!(finished.tones.p50 >= baseline.tones.p50 - 1.0);
}

#[test]
fn small_near_clipped_area_does_not_veto_muted_subject_colour() {
    let result = propose(|x, y| {
        if x < 8 && y < 32 {
            [1.0; 3]
        } else {
            let level = 0.06 + 0.025 * y as f32 / 96.0;
            [level * 1.08 / (2600.0 / 1024.0), level, level / (1600.0 / 1024.0)]
        }
    });
    assert!(result.params.vibrance >= 10.0, "a 2% white region must not veto colour in the other 98%");
}

#[test]
fn backlit_subject_in_shade_is_visibly_opened() {
    let raw = arw::decode(
        &Synthetic::from_camera_rgb(
            128,
            96,
            |_, y| {
                if y < 24 {
                    [1.0; 3]
                } else {
                    grey(0.012 + 0.005 * y as f32 / 96.0)
                }
            },
        )
        .to_bytes(),
    )
    .unwrap();
    let result = auto::auto(&raw, &Params::default()).unwrap();
    let baseline = rendered(&raw, &Params::default());
    let finished = rendered(&raw, &result.params);
    println!("shade {:?}; {:?}; {:?} -> {:?}", result.params.to_sparse(), result.notes, baseline.zones, finished.zones);
    assert!(result.params.shadows > 0.0);
    assert!(finished.zones[2][1] >= baseline.zones[2][1] + 4.0, "test the shaded subject, not the sky");
    assert!(result.params.exposure >= 0.0);
}

fn camera_file(pixel: impl Fn(usize, usize) -> [f32; 3], jpeg: &[u8], space: Option<u16>) -> Vec<u8> {
    let mut synthetic = Synthetic::from_camera_rgb(128, 96, pixel);
    synthetic.exif_color_space = space;
    synthetic.embedded_jpeg = Some(jpeg.to_vec());
    synthetic.to_bytes()
}

fn camera_jpeg(pixel: impl Fn(usize, usize) -> [f32; 3]) -> Vec<u8> {
    let mut image = rawsmith::image::Image::new(128, 96);
    for y in 0..96 {
        for x in 0..128 {
            image.data[y * 128 + x] = pixel(x, y);
        }
    }
    let mut bytes = Vec::new();
    jpeg_encoder::Encoder::new(&mut bytes, 100)
        .encode(&rawsmith::output::to_rgb8(&image), 128, 96, jpeg_encoder::ColorType::Rgb)
        .unwrap();
    bytes
}

#[test]
fn camera_brightness_floor_changes_exposure_and_is_explained() {
    let jpeg = camera_jpeg(|_, y| [0.40 + 0.25 * y as f32 / 96.0; 3]);
    let file = camera_file(|_, y| grey(0.035 + 0.035 * y as f32 / 96.0), &jpeg, Some(1));
    let raw = arw::decode(&file).unwrap();
    let fallback = auto::auto(&raw, &Params::default()).unwrap();
    let result = auto::auto_with_file(&raw, &Params::default(), &file).unwrap();
    let camera = rawsmith::compare::row(
        &raw,
        &file,
        &[rawsmith::compare::Panel { kind: "camera", params: Params::default(), recipe_file: None }],
        128,
    )
    .unwrap()
    .0;
    let reference = rawsmith::stats::image_stats(&camera, &rawsmith::color::Spaces::new());
    let finished = rendered(&raw, &result.params);
    println!("camera bound {:?}; {:?}", result.params.to_sparse(), result.notes);
    assert!(result.params.exposure > fallback.params.exposure);
    assert!(finished.tones.p50 >= reference.tones.p50 - 2.1);
    assert!(result.notes.iter().any(|n| n.contains("camera sanity bound raised brightness bounds")));
    result.params.validate().unwrap();
    let again = auto::auto_with_file(&raw, &Params::default(), &file).unwrap();
    assert_eq!(result.params.to_sparse(), again.params.to_sparse());
    assert_eq!(result.notes, again.notes);
}

#[test]
fn unknown_or_corrupt_camera_colour_uses_explained_finished_fallback() {
    for (jpeg, space) in [(camera_jpeg(|_, _| [0.8; 3]), None), (vec![1, 2, 3], Some(1))] {
        let file = camera_file(|_, y| grey(0.05 + 0.01 * y as f32 / 96.0), &jpeg, space);
        let raw = arw::decode(&file).unwrap();
        let baseline = auto::auto(&raw, &Params::default()).unwrap();
        let result = auto::auto_with_file(&raw, &Params::default(), &file).unwrap();
        assert_eq!(result.params.to_sparse(), baseline.params.to_sparse());
        assert!(result.notes.iter().any(|n| n.contains("camera tone bound unavailable")));
    }
}

#[test]
fn adobe_rgb_camera_is_converted_before_setting_tone_floor() {
    let jpeg = camera_jpeg(|_, y| [0.48, 0.36 + 0.12 * y as f32 / 96.0, 0.32]);
    let mut synthetic = Synthetic::from_camera_rgb(128, 96, |_, y| grey(0.04 + 0.02 * y as f32 / 96.0));
    synthetic.exif_color_space = Some(65535);
    synthetic.interoperability_index = Some(*b"R03\0");
    synthetic.embedded_jpeg = Some(jpeg);
    let file = synthetic.to_bytes();
    let raw = arw::decode(&file).unwrap();
    let result = auto::auto_with_file(&raw, &Params::default(), &file).unwrap();
    assert!(result
        .notes
        .iter()
        .any(|n| n.contains("camera tone bound: verified sRGB") && n.contains("Adobe RGB (1998) to sRGB")));
    let (camera, _, srgb) = rawsmith::compare::row(
        &raw,
        &file,
        &[rawsmith::compare::Panel { kind: "camera", params: Params::default(), recipe_file: None }],
        128,
    )
    .unwrap();
    assert!(srgb);
    let reference = rawsmith::stats::image_stats(&camera, &rawsmith::color::Spaces::new());
    let finished = rendered(&raw, &result.params);
    assert!(finished.tones.p50 >= reference.tones.p50 - 2.1);
    assert!(finished.tones.p75 >= reference.tones.p75 - 2.1);
}

#[test]
fn camera_tone_floor_respects_recipe_crop_and_orientation() {
    let jpeg = camera_jpeg(|x, _| if x < 64 { [0.3; 3] } else { [0.9; 3] });
    let mut synthetic = Synthetic::from_camera_rgb(128, 96, |_, y| grey(0.04 + 0.02 * y as f32 / 96.0));
    synthetic.orientation = 6;
    synthetic.exif_color_space = Some(1);
    synthetic.embedded_jpeg = Some(jpeg);
    let file = synthetic.to_bytes();
    let raw = arw::decode(&file).unwrap();
    let mut base = Params::default();
    // Clockwise orientation puts the dark left half at the top.
    base.crop.bottom = 0.45;
    let result = auto::auto_with_file(&raw, &base, &file).unwrap();
    assert_eq!(result.params.crop.bottom, base.crop.bottom);
    let bound = result.notes.iter().find(|n| n.starts_with("camera tone bound:")).unwrap();
    assert!(bound.contains("median 41."), "wrong framing lets the bright excluded half set the floor: {bound}");
    assert!(!result.notes.iter().any(|n| n.contains("bound remains unresolved")));
}

#[test]
fn camera_contrast_bound_is_bounded_and_explains_unreachable_floor() {
    let jpeg = camera_jpeg(|_, y| if y < 60 { [0.46; 3] } else { [0.8; 3] });
    let file = camera_file(|_, y| grey(0.07 + 0.025 * y as f32 / 96.0), &jpeg, Some(1));
    let raw = arw::decode(&file).unwrap();
    let result = auto::auto_with_file(&raw, &Params::default(), &file).unwrap();
    println!("contrast bound {:?}; {:?}", result.params.to_sparse(), result.notes);
    assert!(result.params.contrast >= 10.0 && result.params.contrast <= 30.0);
    assert!(result.notes.iter().any(|n| n.contains("requested brightness floor unreachable")));
    assert!(result.params.exposure <= 1.2);
    assert!(result
        .notes
        .iter()
        .any(|n| n.contains("camera upper-midtone contrast bound") && n.contains("unresolved:")));
    result.params.validate().unwrap();
}

#[test]
fn camera_upper_midtone_gap_can_raise_contrast() {
    let jpeg = camera_jpeg(|_, y| if y < 60 { [0.46; 3] } else { [0.60; 3] });
    let file = camera_file(
        |_, y| {
            let level = 0.03 + 0.10 * y as f32 / 96.0;
            [level * 1.25 / (2600.0 / 1024.0), level, level * 0.8 / (1600.0 / 1024.0)]
        },
        &jpeg,
        Some(1),
    );
    let raw = arw::decode(&file).unwrap();
    let fallback = auto::auto(&raw, &Params::default()).unwrap();
    let result = auto::auto_with_file(&raw, &Params::default(), &file).unwrap();
    let baseline = rendered(&raw, &fallback.params);
    let finished = rendered(&raw, &result.params);
    println!(
        "camera contrast {:?} -> {:?}; {:?}",
        fallback.params.to_sparse(),
        result.params.to_sparse(),
        result.notes
    );
    assert!(result.params.contrast > fallback.params.contrast);
    assert!(finished.tones.p75 - finished.tones.p50 > baseline.tones.p75 - baseline.tones.p50 + 0.1);
    assert!(result.params.contrast <= 30.0);
}
