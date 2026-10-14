//! A single comparison row. Declared Adobe RGB camera pixels are converted to
//! sRGB; recipe panels use the existing developing pipeline unchanged.

use std::io::Cursor;
use std::path::PathBuf;

use jpeg_decoder::{Decoder, PixelFormat};
use rayon::prelude::*;
use serde_json::{json, Value};

use crate::arw::RawImage;
use crate::color;
use crate::error::{Error, Result};
use crate::image::Image;
use crate::params::Params;
use crate::pipeline::{self, Size};
use crate::tiff::Tiff;

const SEPARATOR: usize = 2;
const SRGB_PROFILE: &[u8] = include_bytes!("../assets/profiles/sRGB-v2-magic.icc");

pub struct Panel {
    pub kind: &'static str,
    pub params: Params,
    pub recipe_file: Option<PathBuf>,
}

fn exif_space(data: &[u8]) -> Option<(Option<i64>, Option<String>)> {
    let (tiff, first) = Tiff::new(data).ok()?;
    let ifd0 = tiff.ifd(first).ok()?;
    let exif = tiff.ifd(tiff.int(&ifd0, 0x8769)? as u32).ok()?;
    let space = tiff.int(&exif, 0xa001);
    let index = tiff.int(&exif, 0xa005).and_then(|offset| {
        let interop = tiff.ifd(offset as u32).ok()?;
        tiff.string(&interop, 1)
    });
    (space.is_some() || index.is_some()).then_some((space, index))
}

fn declared_exif_space(exif: Option<i64>, interop: Option<&str>) -> &'static str {
    if interop == Some("R03") && exif.is_none_or(|v| v == 65535) {
        "Adobe RGB (1998)"
    } else if (exif == Some(1) && interop.is_none_or(|v| v == "R98"))
        || (interop == Some("R98") && exif.is_none_or(|v| v == 1 || v == 65535))
    {
        "sRGB"
    } else if exif == Some(65535) && interop.is_none() {
        "uncalibrated"
    } else {
        "unknown"
    }
}

fn adobe_rgb_to_srgb(pixel: [f32; 3]) -> [f32; 3] {
    // inv(sRGB-to-XYZ) * Adobe-RGB-to-XYZ, derived from the published xy
    // primaries with the same D65 white (0.3127, 0.3290). No adaptation needed.
    // Adobe's transfer function is a pure power law, not the sRGB curve:
    // https://www.adobe.com/digitalimag/pdfs/AdobeRGB1998.pdf, section 4.3.1.
    const MATRIX: color::Mat3 = [[1.398_355_7, -0.398_355_75, 0.0], [0.0, 1.0, 0.0], [0.0, -0.042_928_99, 1.042_929]];
    let linear = pixel.map(|v| v.powf(563.0 / 256.0));
    // Adobe RGB includes colours outside sRGB. Clip in linear destination RGB
    // rather than changing the camera's tone or applying the recipe gamut map.
    color::apply(&MATRIX, linear).map(|v| color::srgb_encode(v.clamp(0.0, 1.0)))
}

// Detect an ICC declaration even if its chunk sequence cannot be assembled by
// the decoder. An invalid ICC must not fall back to an EXIF sRGB declaration.
fn has_icc_marker(jpeg: &[u8]) -> bool {
    let mut at = 2;
    while jpeg.get(at) == Some(&0xff) {
        while jpeg.get(at) == Some(&0xff) {
            at += 1;
        }
        let Some(&marker) = jpeg.get(at) else { break };
        at += 1;
        if matches!(marker, 0xda | 0xd9) {
            break;
        }
        if matches!(marker, 0x01 | 0xd0..=0xd8) {
            continue;
        }
        let Some(length) = jpeg.get(at..at + 2) else { break };
        let length = u16::from_be_bytes([length[0], length[1]]) as usize;
        if length < 2 {
            break;
        }
        let Some(segment) = jpeg.get(at + 2..at + length) else { break };
        if marker == 0xe2 && segment.starts_with(b"ICC_PROFILE\0") {
            return true;
        }
        at += length;
    }
    false
}

/// `reduce_to` lets the decoder scale within its inverse transform to the
/// smallest of its sizes whose long edge is still at least that many pixels:
/// enough for tone measurements, and several times quicker than a full
/// decode. A comparison panel is always decoded in full.
fn camera(jpeg: &[u8], raw_file: &[u8], reduce_to: Option<usize>) -> Result<(Image, Value)> {
    let _stage = crate::profile::Stage::new("camera reference");
    let mut decoder = Decoder::new(Cursor::new(jpeg));
    decoder.set_max_decoding_buffer_size(128 * 1024 * 1024);
    if let Some(edge) = reduce_to {
        decoder.read_info().map_err(|e| Error::Format(format!("cannot decode embedded JPEG: {e}")))?;
        let info = decoder.info().ok_or_else(|| Error::Format("embedded JPEG has no image information".into()))?;
        let long = info.width.max(info.height).max(1) as f64;
        let k = (edge as f64 / long).min(1.0);
        let request = |v: u16| ((v as f64 * k).ceil() as u16).clamp(1, v.max(1));
        decoder
            .scale(request(info.width), request(info.height))
            .map_err(|e| Error::Format(format!("cannot decode embedded JPEG: {e}")))?;
    }
    let bytes = decoder.decode().map_err(|e| Error::Format(format!("cannot decode embedded JPEG: {e}")))?;
    let info = decoder.info().ok_or_else(|| Error::Format("embedded JPEG has no image information".into()))?;
    let data = match info.pixel_format {
        PixelFormat::RGB24 => bytes.as_chunks::<3>().0.par_iter().map(|p| p.map(|v| v as f32 / 255.0)).collect(),
        PixelFormat::L8 => bytes.par_iter().map(|&v| [v as f32 / 255.0; 3]).collect(),
        _ => return Err(Error::Unsupported("embedded JPEG must be 8-bit RGB or greyscale".into())),
    };
    let mut image = Image { width: info.width as usize, height: info.height as usize, data };
    let icc = decoder.icc_profile();
    let has_icc = icc.is_some() || has_icc_marker(jpeg);
    // Recognise the bundled sRGB profile exactly. Other ICC profiles are
    // conservatively unverified: a profile's name alone is not a colour transform.
    let (source, space) = if has_icc {
        let space = if icc.as_deref() == Some(SRGB_PROFILE) { "sRGB" } else { "ICC (unverified)" };
        ("embedded-jpeg-icc", space)
    } else if let Some((exif, interop)) = decoder.exif_data().and_then(exif_space) {
        ("embedded-jpeg-exif", declared_exif_space(exif, interop.as_deref()))
    } else if let Some((exif, interop)) = exif_space(raw_file) {
        ("raw-exif", declared_exif_space(exif, interop.as_deref()))
    } else {
        ("none", "unknown")
    };
    let conversion = if space == "Adobe RGB (1998)" {
        image.data.par_iter_mut().for_each(|p| *p = adobe_rgb_to_srgb(*p));
        Some("Adobe RGB (1998) to sRGB")
    } else {
        None
    };
    Ok((
        image,
        json!({"color_space": if conversion.is_some() { "sRGB" } else { space },
            "color_declaration": {"source": source, "color_space": space},
            "color_conversion": conversion}),
    ))
}

fn framing(params: &Params) -> Value {
    json!({"rotate": params.rotate, "crop": params.crop})
}

/// Returns the row, its report, and whether every panel is established sRGB.
pub fn row(raw: &RawImage, file: &[u8], panels: &[Panel], size: usize) -> Result<(Image, Value, bool)> {
    row_with(raw, file, panels, size, false, None)
}

/// A row of the same part of every panel, `[x0, y0, x1, y1]` as fractions of
/// each panel's finished frame, taken from native pixels before resizing.
pub fn row_region(
    raw: &RawImage,
    file: &[u8],
    panels: &[Panel],
    size: usize,
    region: [f64; 4],
) -> Result<(Image, Value, bool)> {
    row_with(raw, file, panels, size, false, Some(region))
}

/// The same pixel rounding as `render --region`.
pub fn crop_region(image: &Image, [x0, y0, x1, y1]: [f64; 4]) -> Image {
    let [x0, y0, x1, y1] = [x0, y0, x1, y1].map(|v| v as f32);
    let (w, h) = (image.width as f32, image.height as f32);
    let (x, y) = ((x0 * w) as usize, (y0 * h) as usize);
    let width = (((x1 - x0) * w) as usize).max(1).min(image.width - x.min(image.width - 1));
    let height = (((y1 - y0) * h) as usize).max(1).min(image.height - y.min(image.height - 1));
    image.crop(x.min(image.width - 1), y.min(image.height - 1), width, height)
}

/// The camera rendering for tone measurements: the same framing and colour
/// handling as a comparison panel, decoded no larger than `size` needs.
pub(crate) fn analysis_row(raw: &RawImage, file: &[u8], panels: &[Panel], size: usize) -> Result<(Image, Value, bool)> {
    row_with(raw, file, panels, size, true, None)
}

fn row_with(
    raw: &RawImage,
    file: &[u8],
    panels: &[Panel],
    size: usize,
    reduce_camera: bool,
    region: Option<[f64; 4]>,
) -> Result<(Image, Value, bool)> {
    if panels.is_empty() || size == 0 {
        return Err(Error::Params("compare needs at least one panel and --size at least 1".into()));
    }
    if !matches!(raw.meta.orientation, 1 | 3 | 6 | 8) {
        return Err(Error::Unsupported(format!(
            "compare cannot make RAW orientation {} upright",
            raw.meta.orientation
        )));
    }
    let reference = panels.iter().position(|p| p.kind != "camera");
    let defaults = Params::default();
    let camera_params = reference.map(|i| &panels[i].params).unwrap_or(&defaults);
    let camera_needed = panels.iter().any(|p| p.kind == "camera");
    let camera = if camera_needed {
        let (offset, len) =
            raw.embedded_jpeg.ok_or_else(|| Error::Unsupported("this file has no embedded JPEG".into()))?;
        let jpeg = offset
            .checked_add(len)
            .and_then(|end| file.get(offset..end))
            .ok_or_else(|| Error::Format("embedded JPEG is out of bounds".into()))?;
        let (image, report) = camera(jpeg, file, reduce_camera.then_some(size))?;
        let framed = pipeline::framed(image, raw.meta.orientation, camera_params);
        Some((
            match region {
                Some(r) => crop_region(&framed, r),
                None => framed,
            },
            report,
        ))
    } else {
        None
    };
    let srgb = camera.as_ref().is_none_or(|(_, report)| report["color_space"] == "sRGB");
    // Determine the shared height from geometry before developing any recipe.
    // Never upscale a small camera JPEG or a heavily cropped recipe.
    let height = panels
        .iter()
        .map(|panel| {
            let (w, h) = if panel.kind == "camera" {
                let (image, _) = camera.as_ref().expect("camera requested");
                (image.width, image.height)
            } else {
                let (w, h) =
                    pipeline::framed_dimensions(raw.crop.width, raw.crop.height, raw.meta.orientation, &panel.params);
                match region {
                    Some([x0, y0, x1, y1]) => (
                        (((x1 - x0) as f32 * w as f32) as usize).max(1),
                        (((y1 - y0) as f32 * h as f32) as usize).max(1),
                    ),
                    None => (w, h),
                }
            };
            let scale = (size as f64 / w.max(h) as f64).min(1.0);
            ((h as f64 * scale).round() as usize).max(1)
        })
        .min()
        .expect("at least one panel");
    let panel_width =
        |image: &Image| ((image.width as f64 * height as f64 / image.height as f64).round() as usize).clamp(1, size);
    // Release the native camera buffer before developing the first recipe,
    // even if the camera is requested late or more than once in the row.
    let camera = camera.map(|(image, report)| (image.resized(panel_width(&image), height), report));
    let mut images = Vec::new();
    let mut reports = Vec::new();
    let mut looks = Vec::new();
    let spaces = color::Spaces::new();
    let mut width = SEPARATOR.checked_mul(panels.len() - 1);
    for panel in panels {
        let (image, mut report, params) = if panel.kind == "camera" {
            let (image, report) = camera.as_ref().expect("camera requested");
            (image.clone(), report.clone(), camera_params)
        } else {
            let rendered = pipeline::render(raw, &panel.params, Size::default())?;
            let image = match region {
                Some(r) => crop_region(&rendered.image, r),
                None => rendered.image,
            };
            (
                image.resized(panel_width(&image), height),
                json!({"color_space": "sRGB", "noise_reduction": rendered.noise_reduction}),
                &panel.params,
            )
        };
        report["kind"] = json!(panel.kind);
        report["recipe_file"] = json!(panel.recipe_file);
        report["framing"] = framing(params);
        report["width"] = json!(image.width);
        report["height"] = json!(height);
        width = width.and_then(|total| total.checked_add(image.width));
        looks.push(crate::stats::look(&image, &spaces));
        images.push(image);
        reports.push(report);
    }
    // What each version gave up against the camera's rendering. Only the
    // panel the camera was framed for, and only verified camera colour, can
    // be compared pixel area for pixel area.
    let camera_look = panels.iter().position(|p| p.kind == "camera").filter(|_| srgb);
    let reports_framing: Vec<Value> = reports.iter().map(|report| report["framing"].clone()).collect();
    for (index, report) in reports.iter_mut().enumerate() {
        report["look"] = json!(looks[index]);
        if panels[index].kind == "camera" {
            continue;
        }
        if let Some(camera) = camera_look.filter(|&c| reports_framing[c] == reports_framing[index]) {
            report["losses_against_camera"] = json!(crate::stats::losses(&looks[camera], &looks[index]));
        }
    }
    let width = width.ok_or_else(|| Error::Params("comparison row is too wide".into()))?;
    let pixels = width.checked_mul(height).ok_or_else(|| Error::Params("comparison row is too large".into()))?;
    let mut data = Vec::new();
    data.try_reserve_exact(pixels).map_err(|_| Error::Params("comparison row is too large".into()))?;
    data.resize(pixels, [0.5; 3]);
    let mut row = Image { width, height, data };
    let mut left = 0;
    for image in images {
        for y in 0..height {
            let start = y * width + left;
            row.data[start..start + image.width].copy_from_slice(&image.data[y * image.width..(y + 1) * image.width]);
        }
        left += image.width + SEPARATOR;
    }
    let matched = reports.iter().all(|p| p["framing"] == reports[0]["framing"]);
    let mut warnings = Vec::new();
    if !matched {
        warnings.push("Panels have different crop/rotation; each recipe keeps its own framing. The camera matches the first non-camera panel.");
    }
    if !srgb {
        warnings.push("Camera colour is not established sRGB. Camera pixels are unconverted; the mixed comparison has no colour profile, so camera colours may display incorrectly.");
    }
    Ok((
        row,
        json!({
            "panels": reports, "width": width, "height": height,
            "separator_pixels": SEPARATOR, "raw_orientation": raw.meta.orientation,
            "camera_framing_panel": if camera_needed { reference } else { None },
            "framing_policy": "camera matches first non-camera panel; recipes keep their own framing",
            "framing_matched": matched, "warnings": warnings, "region": region,
        }),
        srgb,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stats::{look, losses};

    #[test]
    fn losses_name_what_an_edit_gave_up_and_stay_quiet_otherwise() {
        let spaces = color::Spaces::new();
        // A dark-to-bright ramp under a blue band; the "edit" variants change one thing each.
        let picture = |black: f32, white: f32, blue: [f32; 3]| {
            let mut image = Image::new(64, 64);
            for y in 0..64 {
                for x in 0..64 {
                    let v = black + (white - black) * x as f32 / 63.0;
                    image.data[y * 64 + x] = if y < 24 { blue } else { [v; 3] };
                }
            }
            look(&image, &spaces)
        };
        let reference = picture(0.02, 0.9, [0.2, 0.4, 0.8]);
        assert!(losses(&reference, &reference).is_empty());
        assert!(
            losses(&reference, &picture(0.0, 0.92, [0.18, 0.4, 0.85])).is_empty(),
            "a slightly richer edit loses nothing"
        );
        let lifted = losses(&reference, &picture(0.25, 0.9, [0.2, 0.4, 0.8]));
        assert!(lifted.iter().any(|n| n.starts_with("black point lifted")), "{lifted:?}");
        assert!(lifted.iter().any(|n| n.starts_with("flatter")), "{lifted:?}");
        let blown = losses(&reference, &picture(0.02, 1.4, [0.2, 0.4, 0.8]));
        assert!(blown.iter().any(|n| n.starts_with("more pure white")), "{blown:?}");
        let washed = losses(&reference, &picture(0.02, 0.9, [0.55, 0.65, 0.8]));
        assert!(washed.iter().any(|n| n.starts_with("blue is weaker and paler")), "{washed:?}");
        let grey = losses(&reference, &picture(0.02, 0.9, [0.45; 3]));
        assert!(grey.iter().any(|n| n.starts_with("blue has nearly gone")), "{grey:?}");
        // The same sky turned cyan (towards aqua), then a slight shift that stays quiet.
        let cyan = losses(&reference, &picture(0.02, 0.9, [0.15, 0.45, 0.75]));
        assert!(cyan.iter().any(|n| n.starts_with("blue has turned towards aqua")), "{cyan:?}");
        let purple = losses(&reference, &picture(0.02, 0.9, [0.3, 0.35, 0.85]));
        assert!(purple.iter().any(|n| n.starts_with("blue has turned towards purple")), "{purple:?}");
        assert!(!losses(&reference, &picture(0.02, 0.9, [0.2, 0.41, 0.8])).iter().any(|n| n.contains("turned")));
    }

    // Frozen pre-streaming implementation: render all panels at native size,
    // derive the height from actual images, then resize each panel once.
    fn native_panels_reference(
        raw: &RawImage,
        file: &[u8],
        panels: &[Panel],
        size: usize,
    ) -> Result<(Image, Value, bool)> {
        if panels.is_empty() || size == 0 {
            return Err(Error::Params("compare needs at least one panel and --size at least 1".into()));
        }
        if !matches!(raw.meta.orientation, 1 | 3 | 6 | 8) {
            return Err(Error::Unsupported(format!(
                "compare cannot make RAW orientation {} upright",
                raw.meta.orientation
            )));
        }
        let reference = panels.iter().position(|p| p.kind != "camera");
        let defaults = Params::default();
        let camera_params = reference.map(|i| &panels[i].params).unwrap_or(&defaults);
        let camera_needed = panels.iter().any(|p| p.kind == "camera");
        let camera = if camera_needed {
            let (offset, len) =
                raw.embedded_jpeg.ok_or_else(|| Error::Unsupported("this file has no embedded JPEG".into()))?;
            let jpeg = offset
                .checked_add(len)
                .and_then(|end| file.get(offset..end))
                .ok_or_else(|| Error::Format("embedded JPEG is out of bounds".into()))?;
            let (image, report) = camera(jpeg, file, None)?;
            Some((pipeline::framed(image, raw.meta.orientation, camera_params), report))
        } else {
            None
        };
        let srgb = camera.as_ref().is_none_or(|(_, report)| report["color_space"] == "sRGB");
        let mut images = Vec::new();
        let mut reports = Vec::new();
        for panel in panels {
            let (image, mut report, params) = if panel.kind == "camera" {
                let (image, report) = camera.as_ref().expect("camera requested");
                (image.clone(), report.clone(), camera_params)
            } else {
                let rendered = pipeline::render(raw, &panel.params, Size::default())?;
                (
                    rendered.image,
                    json!({"color_space": "sRGB", "noise_reduction": rendered.noise_reduction}),
                    &panel.params,
                )
            };
            report["kind"] = json!(panel.kind);
            report["recipe_file"] = json!(panel.recipe_file);
            report["framing"] = framing(params);
            images.push(image);
            reports.push(report);
        }
        // Never upscale a small camera JPEG or a heavily cropped recipe. Taking
        // the smallest bounded height keeps every panel's long edge within --size.
        let height = images
            .iter()
            .map(|image| {
                let scale = (size as f64 / image.width.max(image.height) as f64).min(1.0);
                ((image.height as f64 * scale).round() as usize).max(1)
            })
            .min()
            .expect("at least one panel");
        let mut width = SEPARATOR.checked_mul(images.len() - 1);
        for (image, report) in images.iter_mut().zip(&mut reports) {
            let w = ((image.width as f64 * height as f64 / image.height as f64).round() as usize).clamp(1, size);
            *image = image.resized(w, height);
            report["width"] = json!(w);
            report["height"] = json!(height);
            width = width.and_then(|total| total.checked_add(w));
        }
        let width = width.ok_or_else(|| Error::Params("comparison row is too wide".into()))?;
        let pixels = width.checked_mul(height).ok_or_else(|| Error::Params("comparison row is too large".into()))?;
        let mut data = Vec::new();
        data.try_reserve_exact(pixels).map_err(|_| Error::Params("comparison row is too large".into()))?;
        data.resize(pixels, [0.5; 3]);
        let mut row = Image { width, height, data };
        let mut left = 0;
        for image in images {
            for y in 0..height {
                let start = y * width + left;
                row.data[start..start + image.width]
                    .copy_from_slice(&image.data[y * image.width..(y + 1) * image.width]);
            }
            left += image.width + SEPARATOR;
        }
        let matched = reports.iter().all(|p| p["framing"] == reports[0]["framing"]);
        let mut warnings = Vec::new();
        if !matched {
            warnings.push("Panels have different crop/rotation; each recipe keeps its own framing. The camera matches the first non-camera panel.");
        }
        if !srgb {
            warnings.push("Camera colour is not established sRGB. Camera pixels are unconverted; the mixed comparison has no colour profile, so camera colours may display incorrectly.");
        }
        Ok((
            row,
            json!({
                "panels": reports, "width": width, "height": height,
                "separator_pixels": SEPARATOR, "raw_orientation": raw.meta.orientation,
                "camera_framing_panel": if camera_needed { reference } else { None },
                "framing_policy": "camera matches first non-camera panel; recipes keep their own framing",
                "framing_matched": matched, "warnings": warnings,
            }),
            srgb,
        ))
    }

    #[test]
    fn streamed_panels_match_native_panels_reference_bit_for_bit() {
        use crate::synthetic::Synthetic;

        let mut synthetic = Synthetic::from_camera_rgb(66, 50, |x, y| {
            let texture = ((x * 31 + y * 47) % 97) as f32 / 97.0 * 0.04;
            let v = 0.01 + x as f32 / 66.0 * 0.8 + texture;
            [v * 0.8, v, v * 0.6]
        });
        let mut preview = Image::new(41, 27);
        preview.map_xy(|x, y, _| [x as f32 / 41.0, y as f32 / 27.0, 0.3]);
        synthetic.embedded_jpeg =
            Some(crate::output::encode(std::path::Path::new("camera.jpg"), &preview, 94).unwrap());
        let recipe = |value| serde_json::from_value::<Params>(value).unwrap();
        let params = [
            Params::default(),
            recipe(json!({"rotate": 3.0, "crop": {"top": 0.07, "bottom": 0.93, "left": 0.11}, "denoise": 85})),
            recipe(json!({"rotate": -7.0, "crop": {"top": 0.6}, "exposure": -0.5})),
            recipe(json!({"crop": {"left": 0.99, "top": 0.99}})),
        ];
        for orientation in [1, 3, 6, 8] {
            synthetic.orientation = orientation;
            let file = synthetic.to_bytes();
            let mut raw = crate::arw::decode(&file).unwrap();
            // Predict dimensions from the active area, not the stored frame.
            raw.crop.x = 2;
            raw.crop.y = 2;
            raw.crop.width -= 4;
            raw.crop.height -= 4;
            for indices in [&[1, 2, 0][..], &[1, 4, 2, 4][..], &[3, 0][..], &[4][..]] {
                let panels: Vec<_> = indices
                    .iter()
                    .map(|&i| Panel {
                        kind: if i == 4 { "camera" } else { "recipe" },
                        params: params[i.min(3)].clone(),
                        recipe_file: (i != 4).then(|| PathBuf::from(format!("synthetic-{i}.json"))),
                    })
                    .collect();
                for size in [1, 19, 43, 100] {
                    let (expected, expected_report, expected_srgb) =
                        native_panels_reference(&raw, &file, &panels, size).unwrap();
                    let (actual, mut report, srgb) = row(&raw, &file, &panels, size).unwrap();
                    // The frozen reference predates the look descriptions.
                    for panel in report["panels"].as_array_mut().unwrap() {
                        let panel = panel.as_object_mut().unwrap();
                        assert!(panel.remove("look").is_some());
                        panel.remove("losses_against_camera");
                    }
                    let context = format!("orientation {orientation}, panels {indices:?}, size {size}");
                    assert_eq!((actual.width, actual.height), (expected.width, expected.height), "{context}");
                    let bits = |image: &Image| image.data.iter().map(|p| p.map(f32::to_bits)).collect::<Vec<_>>();
                    assert_eq!(bits(&actual), bits(&expected), "{context}");
                    assert_eq!(report, expected_report, "{context}: report");
                    assert_eq!(srgb, expected_srgb, "{context}: colour space");
                }
            }
        }
    }

    #[test]
    fn exif_colour_walk_handles_big_endian_and_bad_offsets() {
        // IFD0 at 8, ExifIFD at 26, InteropIFD at 56. Offsets are relative to
        // the whole TIFF container, even when used for a RAW rather than APP1.
        let mut file = b"MM\0*\0\0\0\x08".to_vec();
        file.extend(1u16.to_be_bytes());
        file.extend([0x87, 0x69, 0, 4, 0, 0, 0, 1, 0, 0, 0, 26]);
        file.extend(0u32.to_be_bytes());
        file.extend(2u16.to_be_bytes());
        file.extend([0xa0, 1, 0, 3, 0, 0, 0, 1, 0xff, 0xff, 0, 0]);
        file.extend([0xa0, 5, 0, 4, 0, 0, 0, 1, 0, 0, 0, 56]);
        file.extend(0u32.to_be_bytes());
        file.extend(1u16.to_be_bytes());
        file.extend([0, 1, 0, 2, 0, 0, 0, 4, b'R', b'0', b'3', 0]);
        file.extend(0u32.to_be_bytes());
        assert_eq!(exif_space(&file), Some((Some(65535), Some("R03".into()))));
        file[48..52].copy_from_slice(&u32::MAX.to_be_bytes());
        assert_eq!(exif_space(&file), Some((Some(65535), None)));
        file[18..22].copy_from_slice(&u32::MAX.to_be_bytes());
        assert_eq!(exif_space(&file), None);
    }

    #[test]
    fn adobe_rgb_conversion_matches_independent_colour_references() {
        // References calculated in f64 through XYZ using the xy primaries,
        // D65 white, gamma 563/256, and the destination sRGB transfer function.
        for (input, expected) in [
            ([0.0; 3], [0.0; 3]),
            ([1.0; 3], [1.0; 3]),
            ([0.5; 3], [0.503_992_9; 3]),
            ([0.03; 3], [0.005_782_545; 3]),
            ([0.4, 0.3, 0.2], [0.434_314_9, 0.295_039_6, 0.180_080_38]),
            ([1.0, 0.0, 0.0], [1.0, 0.0, 0.0]),
            ([0.0, 1.0, 0.0], [0.0, 1.0, 0.0]),
            ([0.0, 0.0, 1.0], [0.0, 0.0, 1.0]),
        ] {
            let got = adobe_rgb_to_srgb(input);
            assert!(got.iter().zip(expected).all(|(a, b)| (a - b).abs() < 2e-6), "{input:?}: {got:?}");
        }
    }

    #[test]
    fn conversion_is_applied_to_decoded_camera_pixels() {
        let image = Image { width: 8, height: 8, data: vec![[0.4, 0.3, 0.2]; 64] };
        let jpeg =
            crate::output::encode_with_color_space(std::path::Path::new("camera.jpg"), &image, 100, false).unwrap();
        let mut synthetic = crate::synthetic::Synthetic::new(16, 16, vec![600; 256]);
        synthetic.exif_color_space = Some(65535);
        synthetic.interoperability_index = Some(*b"R03\0");
        let file = synthetic.to_bytes();
        let (converted, report) = camera(&jpeg, &file, None).unwrap();
        let (unconverted, _) = camera(&jpeg, &[], None).unwrap();
        // Encoding a solid patch introduces at most a little JPEG rounding.
        for pixel in &converted.data {
            assert!(pixel.iter().zip([0.434_314_9, 0.295_039_6, 0.180_080_38]).all(|(a, b)| (a - b).abs() < 0.01));
        }
        assert!((converted.data[0][0] - unconverted.data[0][0]).abs() > 0.025);
        assert_eq!(report["color_space"], "sRGB");
    }

    #[test]
    fn reduced_camera_decoding_keeps_tones_and_never_goes_below_the_requested_edge() {
        // Smooth tones with a bright quarter, as a tone measurement sees them.
        let (w, h) = (640, 400);
        let mut image = Image::new(w, h);
        image.map_xy(|x, y, _| {
            let v = 0.15 + 0.5 * x as f32 / w as f32 + if y < h / 4 { 0.25 } else { 0.0 };
            [v, v * 0.9, v * 0.8]
        });
        let jpeg =
            crate::output::encode_with_color_space(std::path::Path::new("camera.jpg"), &image, 92, true).unwrap();
        let (full, full_report) = camera(&jpeg, &[], None).unwrap();
        assert_eq!((full.width, full.height), (w, h));
        for (edge, expected) in [
            (640, (640, 400)),
            (2000, (640, 400)),
            (321, (640, 400)),
            (300, (320, 200)),
            (161, (320, 200)),
            (160, (160, 100)),
            (80, (80, 50)),
            (1, (80, 50)),
        ] {
            let (reduced, report) = camera(&jpeg, &[], Some(edge)).unwrap();
            assert_eq!((reduced.width, reduced.height), expected, "edge {edge}");
            assert_eq!(report, full_report);
            // The same picture at the same size, within JPEG rounding.
            let reference = full.resized(reduced.width, reduced.height);
            let worst = reduced
                .data
                .iter()
                .zip(&reference.data)
                .flat_map(|(a, b)| (0..3).map(move |c| (a[c] - b[c]).abs()))
                .fold(0.0, f32::max);
            assert!(worst < 0.02, "edge {edge}: {worst}");
        }
    }
}
