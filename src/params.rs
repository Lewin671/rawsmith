//! The edit recipe: every adjustment that turns a RAW file into a picture.
//!
//! A recipe is plain JSON stored next to the RAW (`photo.ARW.rawsmith.json`).
//! All fields are optional and default to a finished rendering, so an empty
//! object is a valid recipe. Slider-style fields follow the conventions of
//! mainstream raw developers (-100..100, 0 = no change).

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::error::{Error, Result};

pub const HSL_BANDS: [&str; 8] = ["red", "orange", "yellow", "green", "aqua", "blue", "purple", "magenta"];

#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Band {
    pub hue: f64,
    pub saturation: f64,
    pub luminance: f64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Hsl {
    pub red: Band,
    pub orange: Band,
    pub yellow: Band,
    pub green: Band,
    pub aqua: Band,
    pub blue: Band,
    pub purple: Band,
    pub magenta: Band,
}

impl Hsl {
    pub fn bands(&self) -> [Band; 8] {
        [self.red, self.orange, self.yellow, self.green, self.aqua, self.blue, self.purple, self.magenta]
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Tint {
    pub hue: f64,
    pub saturation: f64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Grade {
    pub shadows: Tint,
    pub highlights: Tint,
    pub balance: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Crop {
    pub left: f64,
    pub top: f64,
    pub right: f64,
    pub bottom: f64,
}

impl Default for Crop {
    fn default() -> Self {
        Crop { left: 0.0, top: 0.0, right: 1.0, bottom: 1.0 }
    }
}

/// A graduated filter: a local adjustment at full strength at (and beyond)
/// `from`, fading smoothly to nothing at `to`. Points are `[x, y]` fractions
/// of the final picture, origin at the top left.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Gradient {
    pub from: [f64; 2],
    pub to: [f64; 2],
    #[serde(default)]
    pub exposure: f64,
    #[serde(default)]
    pub saturation: f64,
}

/// One local selection multiplied by adjustments. Geometry is in the finished frame.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MaskShape {
    #[default]
    Linear,
    Radial,
}

fn default_feather() -> f64 {
    0.5
}
fn default_luma_feather() -> f64 {
    0.05
}
fn is_zero(v: &f64) -> bool {
    *v == 0.0
}
fn is_false(v: &bool) -> bool {
    !*v
}
fn is_linear(v: &MaskShape) -> bool {
    *v == MaskShape::Linear
}
fn is_default_feather(v: &f64) -> bool {
    *v == default_feather()
}
fn is_default_luma_feather(v: &f64) -> bool {
    *v == default_luma_feather()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LocalMask {
    #[serde(skip_serializing_if = "is_linear")]
    pub shape: MaskShape,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from: Option<[f64; 2]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to: Option<[f64; 2]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub center: Option<[f64; 2]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub radii: Option<[f64; 2]>,
    #[serde(skip_serializing_if = "is_zero")]
    pub rotation: f64,
    #[serde(skip_serializing_if = "is_default_feather")]
    pub feather: f64,
    #[serde(skip_serializing_if = "is_false")]
    pub invert: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub luma: Option<[f64; 2]>,
    #[serde(skip_serializing_if = "is_default_luma_feather")]
    pub luma_feather: f64,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub colors: Vec<String>,
    #[serde(skip_serializing_if = "is_zero")]
    pub exposure: f64,
    #[serde(skip_serializing_if = "is_zero")]
    pub saturation: f64,
    #[serde(skip_serializing_if = "is_zero")]
    pub contrast: f64,
    #[serde(skip_serializing_if = "is_zero")]
    pub shadows: f64,
    #[serde(skip_serializing_if = "is_zero")]
    pub highlights: f64,
}

impl Default for LocalMask {
    fn default() -> Self {
        Self {
            shape: MaskShape::Linear,
            from: None,
            to: None,
            center: None,
            radii: None,
            rotation: 0.0,
            feather: default_feather(),
            invert: false,
            luma: None,
            luma_feather: default_luma_feather(),
            colors: Vec::new(),
            exposure: 0.0,
            saturation: 0.0,
            contrast: 0.0,
            shadows: 0.0,
            highlights: 0.0,
        }
    }
}

impl LocalMask {
    pub fn validate(&self) -> Result<()> {
        let fail = |message: &str| Error::Params(format!("invalid mask: {message}"));
        let point = |p: [f64; 2]| p.iter().all(|v| (0.0..=1.0).contains(v));
        match self.shape {
            MaskShape::Linear => {
                if self.center.is_some()
                    || self.radii.is_some()
                    || self.rotation != 0.0
                    || self.feather != default_feather()
                {
                    return Err(fail("center, radii, rotation and feather require shape radial"));
                }
                match (self.from, self.to) {
                    (Some(a), Some(b)) if point(a) && point(b) && (a[0] - b[0]).hypot(a[1] - b[1]) >= 0.02 => {}
                    (None, None) if self.luma.is_some() || !self.colors.is_empty() => {}
                    _ => {
                        return Err(fail(
                            "linear requires from/to points in 0..1 at least 0.02 apart; omit both for luma or colors alone",
                        ))
                    }
                }
            }
            MaskShape::Radial => {
                if self.from.is_some() || self.to.is_some() {
                    return Err(fail("radial cannot have from/to"));
                }
                if !self.center.is_some_and(point)
                    || !self.radii.is_some_and(|r| r.iter().all(|v| (0.01..=2.0).contains(v)))
                {
                    return Err(fail("radial requires center in 0..1 and radii in 0.01..2 (finished-frame fractions)"));
                }
                if !(-180.0..=180.0).contains(&self.rotation) || !(0.01..=1.0).contains(&self.feather) {
                    return Err(fail("rotation must be -180..180 degrees and feather 0.01..1 of the ellipse radius"));
                }
            }
        }
        if let Some([low, high]) = self.luma {
            if !(0.0..=1.0).contains(&low) || !(0.0..=1.0).contains(&high) || low >= high {
                return Err(fail("luma requires [low, high] in 0..1 with low < high"));
            }
        } else if self.luma_feather != default_luma_feather() {
            return Err(fail("luma_feather requires luma"));
        }
        if !(0.02..=0.5).contains(&self.luma_feather) {
            return Err(fail("luma_feather must be 0.02..0.5 display lightness"));
        }
        for (i, colour) in self.colors.iter().enumerate() {
            if !HSL_BANDS.contains(&colour.as_str()) {
                return Err(fail(&format!(
                    "unknown color '{colour}'; colors are HSL band names: {}",
                    HSL_BANDS.join(", ")
                )));
            }
            if self.colors[..i].contains(colour) {
                return Err(fail(&format!("color '{colour}' is listed twice")));
            }
        }
        if !(-3.0..=3.0).contains(&self.exposure) {
            return Err(fail("exposure must be -3..3 EV"));
        }
        for (name, v) in [
            ("saturation", self.saturation),
            ("contrast", self.contrast),
            ("shadows", self.shadows),
            ("highlights", self.highlights),
        ] {
            if !(-100.0..=100.0).contains(&v) {
                return Err(fail(&format!("{name} must be -100..100")));
            }
        }
        Ok(())
    }
}

pub const MAX_MASKS: usize = 8;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Params {
    pub temperature: Option<f64>,
    pub tint: Option<f64>,
    pub exposure: f64,
    pub contrast: f64,
    pub highlights: f64,
    pub shadows: f64,
    pub whites: f64,
    pub blacks: f64,
    pub clarity: f64,
    pub dehaze: f64,
    pub vibrance: f64,
    pub saturation: f64,
    pub hsl: Hsl,
    pub grade: Grade,
    pub curve: Vec<[f64; 2]>,
    pub gradients: Vec<Gradient>,
    pub masks: Vec<LocalMask>,
    pub vignette: f64,
    pub sharpen: f64,
    pub denoise: Option<f64>,
    pub defringe: f64,
    pub lens_corrections: bool,
    pub rotate: f64,
    pub crop: Crop,
}

impl Default for Params {
    fn default() -> Self {
        Params {
            temperature: None,
            tint: None,
            exposure: 0.0,
            contrast: 0.0,
            highlights: 0.0,
            shadows: 0.0,
            whites: 0.0,
            blacks: 0.0,
            clarity: 0.0,
            dehaze: 0.0,
            vibrance: 0.0,
            saturation: 0.0,
            hsl: Hsl::default(),
            grade: Grade::default(),
            curve: Vec::new(),
            gradients: Vec::new(),
            masks: Vec::new(),
            vignette: 0.0,
            sharpen: 40.0,
            denoise: None,
            defringe: 0.0,
            lens_corrections: true,
            rotate: 0.0,
            crop: Crop::default(),
        }
    }
}

/// One numeric field: its dotted path, allowed range and meaning.
pub struct Field {
    pub path: String,
    pub min: f64,
    pub max: f64,
    pub doc: &'static str,
}

fn field(path: &str, min: f64, max: f64, doc: &'static str) -> Field {
    Field { path: path.to_string(), min, max, doc }
}

/// Every numeric field of a recipe. This table drives validation and the
/// `schema` command, so it is the single description of the parameter space.
pub fn fields() -> Vec<Field> {
    let mut out = vec![
        field(
            "temperature",
            2000.0,
            15000.0,
            "White balance colour temperature in kelvin; higher renders warmer. null keeps the camera's as-shot value.",
        ),
        field(
            "tint",
            -150.0,
            150.0,
            "White balance tint; positive adds magenta, negative adds green. null keeps the as-shot value.",
        ),
        field(
            "exposure",
            -5.0,
            5.0,
            "Overall brightness in stops (EV), applied to scene-linear light before tone mapping.",
        ),
        field("contrast", -100.0, 100.0, "Global contrast around middle grey."),
        field(
            "highlights",
            -100.0,
            100.0,
            "Local brightness of bright areas; negative darkens skies and sunlit snow while keeping available detail. Sensor-saturated detail cannot be recovered.",
        ),
        field("shadows", -100.0, 100.0, "Local brightness of dark areas; positive opens foreground silhouettes."),
        field(
            "whites",
            -100.0,
            100.0,
            "White point; positive stretches bright tones up towards pure white (snow, fog, clouds), negative pulls them down.",
        ),
        field("blacks", -100.0, 100.0, "Black point; negative deepens the darkest tones, positive lifts them."),
        field("clarity", -100.0, 100.0, "Mid-scale local contrast; positive adds punch, negative softens."),
        field("dehaze", 0.0, 100.0, "Removes atmospheric haze and restores contrast and colour in distant scenery."),
        field(
            "vibrance",
            -100.0,
            100.0,
            "Saturation change weighted towards muted colours; protects already vivid colours and skin tones.",
        ),
        field("saturation", -100.0, 100.0, "Uniform saturation change; -100 is monochrome."),
        field("grade.shadows.hue", 0.0, 360.0, "Hue (degrees) of the colour tint added to shadows."),
        field("grade.shadows.saturation", 0.0, 100.0, "Strength of the shadow tint."),
        field("grade.highlights.hue", 0.0, 360.0, "Hue (degrees) of the colour tint added to highlights."),
        field("grade.highlights.saturation", 0.0, 100.0, "Strength of the highlight tint."),
        field(
            "grade.balance",
            -100.0,
            100.0,
            "Shifts the shadow/highlight split of the colour grade; positive favours highlights.",
        ),
        field("vignette", -100.0, 100.0, "Brightens (positive) or darkens (negative) the corners, on top of the lens correction."),
        field("sharpen", 0.0, 100.0, "Native-resolution output sharpening amount; previews resize after sharpening."),
        field("defringe", 0.0, 100.0, "Additional suppression of thin purple/blue chroma outliers at strong luminance edges and of coloured specks on bright dots between fine dark structure, on top of automatic neutral-edge demosaic protection; 0 disables only the additional filter. Preserves broad coloured subjects, but may affect genuinely coloured detail only one or two sensor pixels wide (purple lines, distant coloured lamps). Check at full resolution."),
        field("denoise", 0.0, 100.0, "Noise reduction strength (chroma grain first, luminance grain from 10 to full effect at 40, broader chroma above 25; higher values smooth more fine texture). null or omitted measures native sensor noise with an ISO prior; an explicit number is exact, including 0; below the measured strength it keeps an even share of fine grain rather than thresholding into the noise. Read noise_reduction.strength in render reports before adjusting."),
        field(
            "rotate",
            -45.0,
            45.0,
            "Straighten: clockwise rotation in degrees; the frame is cropped to stay rectangular.",
        ),
        field("crop.left", 0.0, 1.0, "Left edge position as a fraction of the upright width: 0.1 removes the left tenth."),
        field("crop.top", 0.0, 1.0, "Top edge position as a fraction of the upright height: 0.1 removes the top tenth."),
        field("crop.right", 0.0, 1.0, "Right edge position from the left, as a fraction of the upright width: 0.9 removes the right tenth."),
        field("crop.bottom", 0.0, 1.0, "Bottom edge position from the top, as a fraction of the upright height: 0.9 removes the bottom tenth."),
    ];
    for band in HSL_BANDS {
        out.push(Field {
            path: format!("hsl.{band}.hue"),
            min: -100.0,
            max: 100.0,
            doc: "Shifts this colour range's hue: positive towards the next band in red, orange, yellow, green, aqua, blue, purple, magenta (yellow towards green, blue towards purple), negative towards the previous (yellow towards orange, blue towards aqua).",
        });
        out.push(Field {
            path: format!("hsl.{band}.saturation"),
            min: -100.0,
            max: 100.0,
            doc: "Saturation of this colour range.",
        });
        out.push(Field {
            path: format!("hsl.{band}.luminance"),
            min: -100.0,
            max: 100.0,
            doc: "Brightness of this colour range.",
        });
    }
    out
}

fn lookup<'a>(value: &'a Value, path: &str) -> Option<&'a Value> {
    path.split('.').try_fold(value, |v, key| v.get(key))
}

impl Params {
    pub fn from_json(text: &str) -> Result<Params> {
        let value: Value =
            serde_json::from_str(text).map_err(|e| Error::Params(format!("recipe is not valid JSON: {e}")))?;
        Params::from_value(value)
    }

    pub fn from_value(value: Value) -> Result<Params> {
        if let Some(masks) = value.get("masks") {
            if !masks.as_array().is_some_and(|entries| entries.iter().all(Value::is_object)) {
                return Err(Error::Params("masks requires an array of mask JSON objects".into()));
            }
        }
        let params: Params =
            serde_json::from_value(value).map_err(|e| Error::Params(format!("invalid recipe: {e}")))?;
        params.validate()?;
        Ok(params)
    }

    pub fn to_value(&self) -> Value {
        serde_json::to_value(self).expect("recipes always serialise")
    }

    pub fn validate(&self) -> Result<()> {
        let value = self.to_value();
        for f in fields() {
            let Some(v) = lookup(&value, &f.path).and_then(Value::as_f64) else { continue };
            if !v.is_finite() || v < f.min || v > f.max {
                return Err(Error::Params(format!("{} = {v} is outside {}..{}", f.path, f.min, f.max)));
            }
        }
        if self.crop.right - self.crop.left < 0.05 || self.crop.bottom - self.crop.top < 0.05 {
            return Err(Error::Params(
                "crop must keep at least 5% of each dimension, with left < right and top < bottom".into(),
            ));
        }
        if !self.curve.is_empty() {
            let ordered = self.curve.windows(2).all(|w| w[1][0] > w[0][0]);
            let in_range = self.curve.iter().flatten().all(|v| (0.0..=1.0).contains(v));
            if !ordered || !in_range {
                return Err(Error::Params(
                    "curve needs [input, output] points in 0..1 with strictly increasing inputs".into(),
                ));
            }
        }
        if self.masks.len() > MAX_MASKS {
            return Err(Error::Params("at most 8 masks are supported".into()));
        }
        for (i, mask) in self.masks.iter().enumerate() {
            mask.validate().map_err(|e| Error::Params(format!("masks[{i}]: {e}")))?;
        }
        if self.gradients.len() > 4 {
            return Err(Error::Params("at most 4 gradients are supported".into()));
        }
        for g in &self.gradients {
            let points_ok = g.from.iter().chain(&g.to).all(|v| (0.0..=1.0).contains(v));
            let length = ((g.to[0] - g.from[0]).powi(2) + (g.to[1] - g.from[1]).powi(2)).sqrt();
            if !points_ok
                || length < 0.02
                || !(-3.0..=3.0).contains(&g.exposure)
                || !(-100.0..=100.0).contains(&g.saturation)
            {
                return Err(Error::Params(
                    "each gradient needs from/to points in 0..1 at least 0.02 apart, exposure in -3..3 and saturation in -100..100".into(),
                ));
            }
        }
        Ok(())
    }

    /// Applies assignments: `exposure=0.5`, `hsl.blue.saturation=-20`,
    /// `temperature=null`, `curve=[[0,0],[1,1]]` set a value;
    /// `exposure+=0.3` and `contrast-=10` change the current one;
    /// `gradients+={...}` appends one filter, `gradients-=1` removes index 1 (zero-based).
    pub fn with_assignments(&self, assignments: &[String]) -> Result<Params> {
        let mut root = self.to_value();
        for assignment in assignments {
            let (path, text) = assignment
                .split_once('=')
                .ok_or_else(|| Error::Params(format!("'{assignment}' is not of the form path=value")))?;
            let (path, step) = match path.trim() {
                p if p.ends_with('+') => (p.strip_suffix('+').unwrap().trim(), 1.0),
                p if p.ends_with('-') => (p.strip_suffix('-').unwrap().trim(), -1.0),
                p => (p, 0.0),
            };
            let text = text.trim();
            if path == "masks" {
                let list = root["masks"].as_array_mut().expect("masks is an array");
                if step > 0.0 {
                    let value: Value = serde_json::from_str(text)
                        .map_err(|e| Error::Params(format!("masks+= requires one mask JSON object: {e}")))?;
                    if !value.is_object() {
                        return Err(Error::Params(
                            "masks+= requires one mask JSON object, not an array or scalar".into(),
                        ));
                    }
                    let mask: LocalMask = serde_json::from_value(value)
                        .map_err(|e| Error::Params(format!("masks+= requires one mask JSON object: {e}")))?;
                    mask.validate()?;
                    if list.len() >= MAX_MASKS {
                        return Err(Error::Params("at most 8 masks are supported".into()));
                    }
                    list.push(json!(mask));
                } else if step < 0.0 {
                    let index = text
                        .parse::<usize>()
                        .ok()
                        .filter(|_| !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit()))
                        .ok_or_else(|| {
                            Error::Params("masks-= requires a zero-based non-negative integer index".into())
                        })?;
                    if index >= list.len() {
                        return Err(Error::Params(format!(
                            "mask index {index} is out of range for {} masks",
                            list.len()
                        )));
                    }
                    list.remove(index);
                } else {
                    let value: Value =
                        serde_json::from_str(text).map_err(|e| Error::Params(format!("invalid masks list: {e}")))?;
                    if !value.as_array().is_some_and(|entries| entries.iter().all(Value::is_object)) {
                        return Err(Error::Params("masks requires an array of mask JSON objects".into()));
                    }
                    let masks: Vec<LocalMask> =
                        serde_json::from_value(value).map_err(|e| Error::Params(format!("invalid masks list: {e}")))?;
                    Params { masks: masks.clone(), ..Params::default() }.validate()?;
                    *list = masks.into_iter().map(|m| json!(m)).collect();
                }
                continue;
            }
            if path == "gradients" && step != 0.0 {
                let list = root["gradients"].as_array_mut().expect("gradients is an array");
                if step > 0.0 {
                    let value: Value = serde_json::from_str(text).map_err(|e| {
                        Error::Params(format!("'{assignment}': gradients+= requires one gradient JSON object: {e}"))
                    })?;
                    if !value.is_object() {
                        return Err(Error::Params(format!(
                            "'{assignment}': gradients+= requires one gradient JSON object, not an array or scalar"
                        )));
                    }
                    let gradient: Gradient = serde_json::from_value(value).map_err(|e| {
                        Error::Params(format!("'{assignment}': gradients+= requires one gradient JSON object: {e}"))
                    })?;
                    Params { gradients: vec![gradient], ..Params::default() }.validate()?;
                    if list.len() >= 4 {
                        return Err(Error::Params("cannot append gradient: at most 4 gradients are supported".into()));
                    }
                    list.push(json!(gradient));
                } else {
                    let index = text.parse::<usize>().ok().filter(|_| {
                        !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit())
                    }).ok_or_else(|| Error::Params(format!(
                        "'{assignment}': gradients-= requires a zero-based non-negative integer index (e.g. gradients-=0)"
                    )))?;
                    if index >= list.len() {
                        return Err(Error::Params(format!(
                            "gradient index {index} is out of range for {} gradients; indices are zero-based",
                            list.len()
                        )));
                    }
                    list.remove(index);
                }
                continue;
            }
            let mut new: Value = serde_json::from_str(text.trim_start_matches('+')).map_err(|_| {
                Error::Params(format!("'{text}' is not valid JSON; use a number, true/false, null, or an array (object keys and strings need double quotes)"))
            })?;
            if path == "gradients" {
                let gradients = serde_json::from_value::<Vec<Gradient>>(new.clone())
                    .map_err(|e| Error::Params(format!("invalid gradients list: {e}")))?;
                Params { gradients, ..Params::default() }.validate()?;
            }
            let keys: Vec<&str> = path.split('.').collect();
            let mut node = &mut root;
            for key in &keys[..keys.len() - 1] {
                node = node.get_mut(*key).ok_or_else(|| unknown(path))?;
            }
            let slot =
                node.as_object_mut().and_then(|o| o.get_mut(keys[keys.len() - 1])).ok_or_else(|| unknown(path))?;
            if slot.is_object() {
                // `grade.shadows={"hue":200,"saturation":15}` sets those fields
                // and leaves the group's others as they were.
                if let (0.0, Some(fields)) = (step, new.as_object()) {
                    for (key, value) in fields {
                        let field = slot.get_mut(key).ok_or_else(|| unknown(&format!("{path}.{key}")))?;
                        if field.is_object() {
                            return Err(Error::Params(format!("'{path}.{key}' is a group; set its fields one by one")));
                        }
                        *field = value.clone();
                    }
                    continue;
                }
                return Err(Error::Params(format!(
                    "'{path}' is a group; set one of its fields, e.g. {path}.{}, or several as {path}={{...}}",
                    first_key(slot)
                )));
            }
            if step != 0.0 {
                match (slot.as_f64(), new.as_f64()) {
                    (Some(old), Some(delta)) => {
                        let sum = old + step * delta;
                        let scaled = sum * 1e6;
                        // Non-finite numbers become JSON null, which would silently
                        // clear optional white balance values instead of rejecting them.
                        if !sum.is_finite() || !scaled.is_finite() {
                            return Err(Error::Params(format!("'{assignment}': numeric adjustment overflowed")));
                        }
                        // Round away binary-fraction noise such as 0.7000000000000001.
                        new = json!(scaled.round() / 1e6);
                    }
                    _ => {
                        return Err(Error::Params(format!(
                            "'{assignment}': += and -= need a numeric parameter that currently has a value"
                        )))
                    }
                }
            }
            *slot = new;
        }
        Params::from_value(root)
    }

    /// The recipe with fields equal to their defaults removed, for compact
    /// storage and display.
    pub fn to_sparse(&self) -> Value {
        fn prune(value: &Value, default: &Value) -> Option<Value> {
            match (value, default) {
                (Value::Object(v), Value::Object(d)) => {
                    let kept: Map<String, Value> =
                        v.iter().filter_map(|(k, x)| prune(x, &d[k]).map(|x| (k.clone(), x))).collect();
                    (!kept.is_empty()).then_some(Value::Object(kept))
                }
                _ => (value != default).then(|| value.clone()),
            }
        }
        prune(&self.to_value(), &Params::default().to_value()).unwrap_or_else(|| json!({}))
    }
}

fn unknown(path: &str) -> Error {
    Error::Params(format!("unknown parameter '{path}'; run `rawsmith schema` for the list"))
}

fn first_key(value: &Value) -> String {
    value.as_object().and_then(|o| o.keys().next().cloned()).unwrap_or_default()
}

/// Machine-readable description of the recipe format.
pub fn schema() -> Value {
    let defaults = Params::default().to_value();
    let numeric: Vec<Value> = fields()
        .iter()
        .map(|f| json!({"path": f.path, "min": f.min, "max": f.max, "default": lookup(&defaults, &f.path), "description": f.doc}))
        .collect();
    json!({
        "parameters": numeric,
        "other": [
            {"path": "curve", "default": [], "description": "Optional tone curve applied after tone mapping: [[input, output], ...] points in 0..1 on display brightness, strictly increasing inputs, joined smoothly. [0,0] and [1,1] are implied when not given. Points pull their whole neighbourhood, so pin the tones you want unchanged, e.g. lift only shadows with [[0.15,0.2],[0.4,0.4]]. Empty means none."},
            {"path": "gradients", "default": [], "description": "Up to 4 graduated filters, e.g. [{\"from\":[0.5,0],\"to\":[0.5,0.55],\"exposure\":-0.7,\"saturation\":15}] darkens and enriches a sky: full effect at and beyond `from`, fading to none at `to`; points are [x, y] fractions of the final picture from the top left. exposure is in stops (-3..3), saturation -100..100. --set gradients=[...] replaces the list; gradients+={...} appends one object; gradients-=INDEX removes a zero-based current-list index. Assignments run in order."},
            {"path": "masks", "default": [], "max_entries": MAX_MASKS, "description": "Local shape times luminance and colour selection. masks=[...] replaces; masks+={...} appends; masks-=INDEX removes a zero-based index. See local_masks for fields and ranges; render --show-mask INDEX checks placement."},
            {"path": "lens_corrections", "default": true, "description": "Apply the camera's embedded distortion, vignetting and chromatic aberration corrections."},
        ],
        "hsl_bands": HSL_BANDS,
        "local_masks": mask_schema(),
    })
}

/// Local entry ranges and meanings, separate from global numeric assignments.
fn mask_schema() -> Value {
    json!({
        "max_entries": MAX_MASKS,
        "fields": [
            {"path":"shape", "default":"linear", "values":["linear","radial"], "description":"Linear from/to fade, or a rotated ellipse; omit geometry for luma alone."},
            {"path":"from", "min":0,"max":1,"description":"[x,y] full-strength point in the finished rotated/cropped frame; linear only."},
            {"path":"to", "min":0,"max":1,"description":"[x,y] zero-strength point, at least 0.02 from from; linear only."},
            {"path":"center", "min":0,"max":1,"description":"Radial ellipse centre [x,y] in finished-frame fractions."},
            {"path":"radii", "min":0.01,"max":2,"description":"Ellipse semi-axes [horizontal,vertical] as fractions of finished width/height before rotation."},
            {"path":"rotation", "min":-180,"max":180,"default":0,"description":"Clockwise ellipse rotation in degrees, in pixel space."},
            {"path":"feather", "min":0.01,"max":1,"default":0.5,"description":"Radial fade occupies this fraction of the radius inside the ellipse; outer boundary is zero."},
            {"path":"invert", "default":false,"description":"Invert the shape (outside ellipse); without a shape invert the luma/colors selection."},
            {"path":"luma", "min":0,"max":1,"description":"[low,high] display Oklab lightness before local edits, black/white points and curve; multiply with shape."},
            {"path":"luma_feather", "min":0.02,"max":0.5,"default":0.05,"description":"Soft lightness transitions outside low/high; endpoints 0 and 1 remain fully selected."},
            {"path":"colors", "values": HSL_BANDS, "default":[],"description":"Select colourful pixels whose hue falls in these HSL bands, e.g. [\"yellow\",\"orange\"] for autumn leaves against a blue sky; neighbouring bands blend, near-neutral pixels are excluded, and edges follow lightness. Multiplies with shape and luma."},
            {"path":"exposure", "min":-3,"max":3,"default":0,"description":"Scene-linear brightness offset in EV, weighted by mask."},
            {"path":"saturation", "min":-100,"max":100,"default":0,"description":"Oklab chroma offset, same multiplier as global saturation."},
            {"path":"contrast", "min":-100,"max":100,"default":0,"description":"Offset to global filmic contrast, clamped to -100..100; mask blends tone and colour rendering."},
            {"path":"shadows", "min":-100,"max":100,"default":0,"description":"Masked shadow brightness using the global edge-aware log-luma base."},
            {"path":"highlights", "min":-100,"max":100,"default":0,"description":"Masked highlight brightness using the global edge-aware log-luma base; cannot recover clipped detail."}
        ]
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_recipe_is_the_default() {
        assert_eq!(Params::from_json("{}").unwrap(), Params::default());
        assert_eq!(Params::default().to_sparse(), json!({}));
    }

    #[test]
    fn a_group_takes_an_object_of_its_fields() {
        let p = Params::default()
            .with_assignments(&[
                r#"grade.shadows={"hue":200,"saturation":15}"#.into(),
                "grade.highlights.hue=40".into(),
            ])
            .unwrap();
        assert_eq!((p.grade.shadows.hue, p.grade.shadows.saturation, p.grade.highlights.hue), (200.0, 15.0, 40.0));
        let p = p.with_assignments(&[r#"grade.shadows={"saturation":5}"#.into()]).unwrap();
        assert_eq!((p.grade.shadows.hue, p.grade.shadows.saturation), (200.0, 5.0));
        for bad in [r#"grade.shadows={"tone":5}"#, r#"grade={"shadows":{"hue":1}}"#, r#"grade.shadows+={"hue":1}"#] {
            assert!(Params::default().with_assignments(&[bad.into()]).is_err(), "{bad}");
        }
        assert!(Params::default().with_assignments(&[r#"grade.shadows={"hue":400}"#.into()]).is_err());
    }

    #[test]
    fn mask_colours_are_band_names_and_stand_alone() {
        let ok = Params::from_json(r#"{"masks":[{"colors":["yellow","orange"],"exposure":0.5}]}"#).unwrap();
        assert_eq!(ok.to_sparse()["masks"][0]["colors"], json!(["yellow", "orange"]));
        for (text, message) in [
            (r#"{"masks":[{"colors":["gold"]}]}"#, "unknown color 'gold'"),
            (r#"{"masks":[{"colors":["red","red"]}]}"#, "listed twice"),
            (r#"{"masks":[{"exposure":1}]}"#, "omit both for luma or colors alone"),
        ] {
            let error = Params::from_json(text).unwrap_err().to_string();
            assert!(error.contains(message), "{error}");
        }
        assert!(Params::default().to_sparse().get("masks").is_none());
    }

    #[test]
    fn every_schema_path_exists_and_defaults_are_in_range() {
        let defaults = Params::default().to_value();
        for f in fields() {
            let v = lookup(&defaults, &f.path).unwrap_or_else(|| panic!("{} missing from Params", f.path));
            if let Some(v) = v.as_f64() {
                assert!(v >= f.min && v <= f.max, "{}", f.path);
            }
        }
    }

    #[test]
    fn assignments_set_validate_and_add() {
        let set = |p: &Params, a: &[&str]| p.with_assignments(&a.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        let d = Params::default();
        let p =
            set(&d, &["exposure=0.5", "hsl.blue.saturation=-10", "temperature=5200", "curve=[[0,0],[0.5,0.6],[1,1]]"])
                .unwrap();
        assert_eq!((p.exposure, p.hsl.blue.saturation, p.temperature, p.curve.len()), (0.5, -10.0, Some(5200.0), 3));
        // Plain assignment is absolute, however often it is repeated.
        assert_eq!(set(&p, &["hsl.blue.saturation=-10"]).unwrap().hsl.blue.saturation, -10.0);
        let p = set(&p, &["exposure+=0.25", "hsl.blue.saturation-=5", "temperature=null"]).unwrap();
        assert_eq!((p.exposure, p.hsl.blue.saturation, p.temperature), (0.75, -15.0, None));
        assert_eq!(p.to_sparse()["exposure"], json!(0.75));
        assert_eq!(set(&d, &["exposure+=0.1", "exposure+=0.2", "exposure-=0.6"]).unwrap().exposure, -0.3);
        let g = set(&d, &[r#"gradients=[{"from":[0.5,0],"to":[0.5,0.5],"exposure":-0.7}]"#]).unwrap();
        assert_eq!((g.gradients.len(), g.gradients[0].exposure, g.gradients[0].saturation), (1, -0.7, 0.0));
        for bad in [
            "exposure=9",
            "nope=1",
            "hsl.blue=3",
            "hsl=1",
            "exposure",
            "contrast=abc",
            "curve=[[0.5,0],[0.2,1]]",
            "temperature+=100",
            "exposure+=4.9",
            "lens_corrections+=1",
        ] {
            assert!(set(&p, &[bad]).is_err(), "{bad} should be rejected");
        }
    }

    #[test]
    fn numeric_adjustment_overflow_does_not_clear_white_balance() {
        let p = Params::from_json(r#"{"temperature":5200,"tint":10}"#).unwrap();
        for assignment in ["temperature+=1e308", "temperature-=1e308", "tint+=1e308", "tint-=1e308"] {
            let err = p.with_assignments(&[assignment.into()]).unwrap_err();
            assert_eq!(err.kind(), "params", "{assignment}");
        }
        // Public Params fields can also contain values large enough to overflow addition.
        let large = Params { temperature: Some(f64::MAX), ..p.clone() };
        assert_eq!(large.with_assignments(&["temperature+=1e308".into()]).unwrap_err().kind(), "params");

        let adjusted = p
            .with_assignments(&[
                "temperature+=100".into(),
                "temperature-=50".into(),
                "tint+=0.1".into(),
                "tint+=0.2".into(),
                "tint-=0.6".into(),
            ])
            .unwrap();
        assert_eq!((adjusted.temperature, adjusted.tint), (Some(5250.0), Some(9.7)));
    }

    #[test]
    fn unknown_fields_are_rejected() {
        assert!(Params::from_json(r#"{"exposrue": 1}"#).is_err());
        assert!(Params::from_json(r#"{"hsl": {"teal": {}}}"#).is_err());
    }
}
