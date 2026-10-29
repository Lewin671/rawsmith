use rawsmith::arw::{self, LensTables};
use rawsmith::params::Params;
use rawsmith::pipeline::{self, Size};
use rawsmith::synthetic::Synthetic;
use rayon::ThreadPoolBuilder;

#[test]
fn lens_correction_and_defringe_run_before_crops_at_full_and_half_size() {
    let mut synthetic = Synthetic::from_camera_rgb(320, 240, |x, y| {
        let d = (y as f32 - 0.7 * x as f32 - 20.0) / 1.22;
        let v = 0.8 - 0.75 * (-0.5 * d * d).exp();
        [v, v, v]
    });
    synthetic.wb_levels = [1024; 4];
    let mut raw = arw::decode(&synthetic.to_bytes()).unwrap();
    raw.lens = Some(LensTables {
        distortion: vec![0; 16],
        ca_red: vec![4000; 16],
        ca_blue: vec![-3000; 16],
        vignetting: vec![0; 16],
    });
    let pools: Vec<_> = [1, 3, 8].map(|n| ThreadPoolBuilder::new().num_threads(n).build().unwrap()).into();
    for half in [false, true] {
        let size = Size { max_edge: half.then_some(160) };
        let params = Params { denoise: Some(0.0), defringe: 100.0, ..Params::default() };
        let full = pools[0].install(|| pipeline::scene(&raw, &params, size).unwrap().image);
        let off = pipeline::scene(&raw, &Params { lens_corrections: false, ..params.clone() }, size).unwrap().image;
        assert!(full.data != off.data, "lens tables must be applied in the half={half} path");
        let cropped_params = Params {
            crop: rawsmith::params::Crop { left: 0.1, top: 0.1, right: 0.9, bottom: 0.9, ..Default::default() },
            ..params
        };
        // 128 selects half demosaic for reduced analysis of the crop too,
        // with no subsequent resize. Output renders always use full demosaic.
        let size = Size { max_edge: half.then_some(128) };
        let expected = full.crop(full.width / 10, full.height / 10, full.width * 8 / 10, full.height * 8 / 10);
        for pool in &pools {
            let got = pool.install(|| pipeline::scene(&raw, &cropped_params, size).unwrap().image);
            assert_eq!((got.width, got.height), (expected.width, expected.height));
            assert!(got.data == expected.data, "crop and Rayon scheduling must not change correction");
        }
    }
}

#[test]
fn active_defringe_with_coarse_denoise_is_deterministic_at_every_output_size() {
    let raw = arw::decode(
        &Synthetic::from_camera_rgb(160, 120, |x, _| {
            if x < 78 {
                [0.015; 3]
            } else if x < 80 {
                [0.16, 0.025, 0.22]
            } else {
                [0.3; 3]
            }
        })
        .to_bytes(),
    )
    .unwrap();
    let params = Params { defringe: 100.0, denoise: Some(85.0), ..Params::default() };
    let pools: Vec<_> = [1, 3, 8].map(|n| ThreadPoolBuilder::new().num_threads(n).build().unwrap()).into();
    let native = pools[0].install(|| pipeline::render(&raw, &params, Size::default()).unwrap().image);
    let off = pipeline::render(&raw, &Params { defringe: 0.0, ..params.clone() }, Size::default()).unwrap().image;
    assert_ne!(native.data, off.data, "exercise active defringe with coarse chroma smoothing");
    for edge in [None, Some(1), Some(40), Some(80), Some(120)] {
        let size = Size { max_edge: edge };
        let expected = size.apply(native.clone());
        let encode = |image: &rawsmith::image::Image| {
            ["synthetic.jpg", "synthetic.png"]
                .map(|path| rawsmith::output::encode(std::path::Path::new(path), image, 94).unwrap())
        };
        let bytes = encode(&expected);
        for pool in &pools {
            let got = pool.install(|| pipeline::render(&raw, &params, size).unwrap().image);
            assert_eq!(got.data, expected.data, "edge={edge:?}, threads={}", pool.current_num_threads());
            assert_eq!(encode(&got), bytes, "exports must ignore Rayon scheduling");
        }
    }
}
