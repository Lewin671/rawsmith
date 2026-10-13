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
    assert!(result.params.vibrance <= 5.0, "a vivid subject must not be hidden by the frame mean");
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
        auto::auto(&raw, &Params::default()).unwrap()
    };
    let clean = make(0.0);
    let noisy = make(0.025);
    println!("same ISO denoise: clean {}, noisy {}", clean.params.denoise, noisy.params.denoise);
    assert!(noisy.params.denoise > clean.params.denoise + 20.0);
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
    assert!(
        report.channel_clipped_pct < baseline.channel_clipped_pct * 0.5,
        "local compression should reduce colour clipping without global darkening"
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
