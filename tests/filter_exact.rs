//! Bitwise controls for filter rewrites, including replicated edges, windows
//! larger than the image, signed zero, and incomplete cache tiles.
use rawsmith::image::{guided_filter, Image, Plane};

fn bits(plane: &Plane) -> Vec<u32> {
    plane.data.iter().map(|v| v.to_bits()).collect()
}

fn original_box(input: &Plane, radius: usize) -> Plane {
    if radius == 0 {
        return input.clone();
    }
    let (w, h) = (input.width, input.height);
    let norm = 1.0 / (2 * radius + 1) as f32;
    let run = |src: &[f32], dst: &mut [f32]| {
        let get = |i: isize| src[i.clamp(0, src.len() as isize - 1) as usize];
        let mut sum: f32 = (-(radius as isize)..=radius as isize).map(get).sum();
        for (i, v) in dst.iter_mut().enumerate() {
            *v = sum * norm;
            sum += get(i as isize + radius as isize + 1) - get(i as isize - radius as isize);
        }
    };
    let mut mid = Plane::new(w, h);
    for (src, dst) in input.data.chunks(w).zip(mid.data.chunks_mut(w)) {
        run(src, dst);
    }
    let mut out = Plane::new(w, h);
    for x in 0..w {
        let src: Vec<_> = (0..h).map(|y| mid.data[y * w + x]).collect();
        let mut dst = vec![0.0; h];
        run(&src, &mut dst);
        for (y, &v) in dst.iter().enumerate() {
            out.data[y * w + x] = v;
        }
    }
    out
}

fn original_guided(guide: &Plane, input: &Plane, radius: usize, eps: f32) -> Plane {
    let mul = |a: &Plane, b: &Plane| Plane {
        width: a.width,
        height: a.height,
        data: a.data.iter().zip(&b.data).map(|(x, y)| x * y).collect(),
    };
    let mg = original_box(guide, radius);
    let mi = original_box(input, radius);
    let gi = original_box(&mul(guide, input), radius);
    let gg = original_box(&mul(guide, guide), radius);
    let mut a = Plane::new(guide.width, guide.height);
    let mut b = a.clone();
    for i in 0..a.data.len() {
        let var = (gg.data[i] - mg.data[i] * mg.data[i]).max(0.0);
        let cov = gi.data[i] - mg.data[i] * mi.data[i];
        a.data[i] = cov / (var + eps.max(f32::EPSILON));
        b.data[i] = mi.data[i] - a.data[i] * mg.data[i];
    }
    let (a, b) = (original_box(&a, radius), original_box(&b, radius));
    Plane {
        width: guide.width,
        height: guide.height,
        data: (0..guide.data.len()).map(|i| a.data[i] * guide.data[i] + b.data[i]).collect(),
    }
}

#[test]
fn filters_keep_original_float_bits() {
    for (w, h) in [(1, 1), (1, 7), (7, 1), (3, 2), (31, 33), (65, 49)] {
        for negative_zero in [false, true] {
            let guide = Plane {
                width: w,
                height: h,
                data: (0..w * h)
                    .map(|i| if negative_zero { -0.0 } else { ((i * 37 % 103) as f32 - 51.0) * 0.013 })
                    .collect(),
            };
            let input = Plane { data: guide.data.iter().map(|v| v * -0.7).collect(), ..guide.clone() };
            for radius in [0, 1, 2, 8, 37] {
                assert_eq!(
                    bits(&guide.box_blur(radius)),
                    bits(&original_box(&guide, radius)),
                    "box {w}x{h}, radius {radius}"
                );
                for eps in [0.0, 0.000401] {
                    assert_eq!(
                        bits(&guided_filter(&guide, &input, radius, eps)),
                        bits(&original_guided(&guide, &input, radius, eps)),
                        "guided {w}x{h}, radius {radius}, eps {eps}"
                    );
                }
            }
            let rgb = Image { width: w, height: h, data: guide.data.iter().map(|&v| [v, 0.0, 0.0]).collect() };
            for (ow, oh) in [(w, h), (1, 1), (3, 5), (23, 17), (w * 2, h * 2)] {
                let reference = rgb.resized(ow, oh);
                assert_eq!(
                    bits(&guide.resized(ow, oh)),
                    reference.data.iter().map(|p| p[0].to_bits()).collect::<Vec<_>>()
                );
            }
        }
    }
}
