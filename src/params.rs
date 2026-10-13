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
        field("defringe", 0.0, 100.0, "Additional suppression of thin purple/blue chroma outliers at strong luminance edges, on top of automatic neutral-edge demosaic protection; 0 disables only the additional filter. Preserves broad purple subjects, but may affect genuinely purple detail only a few sensor pixels wide. Check at full resolution."),
        field("denoise", 0.0, 100.0, "Noise reduction strength (chroma grain first, broader chroma above 25, mild luminance above 50). null or omitted measures native sensor noise with an ISO prior; an explicit number is exact, including 0. Read noise_reduction.strength in render reports before adjusting."),
        field(
            "rotate",
            -45.0,
            45.0,
            "Straighten: clockwise rotation in degrees; the frame is cropped to stay rectangular.",
        ),
        field("crop.left", 0.0, 1.0, "Crop edge as a fraction of the upright image width."),
        field("crop.top", 0.0, 1.0, "Crop edge as a fraction of the upright image height."),
        field("crop.right", 0.0, 1.0, "Crop edge as a fraction of the upright image width."),
        field("crop.bottom", 0.0, 1.0, "Crop edge as a fraction of the upright image height."),
    ];
    for band in HSL_BANDS {
        out.push(Field {
            path: format!("hsl.{band}.hue"),
            min: -100.0,
            max: 100.0,
            doc: "Shifts this colour range's hue towards its neighbours.",
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
    /// `exposure+=0.3` and `contrast-=10` change the current one.
    pub fn with_assignments(&self, assignments: &[String]) -> Result<Params> {
        let mut root = self.to_value();
        for assignment in assignments {
            let (path, text) = assignment
                .split_once('=')
                .ok_or_else(|| Error::Params(format!("'{assignment}' is not of the form path=value")))?;
            let (path, step) = match path.trim() {
                p if p.ends_with('+') => (p.trim_end_matches('+').trim(), 1.0),
                p if p.ends_with('-') => (p.trim_end_matches('-').trim(), -1.0),
                p => (p, 0.0),
            };
            let text = text.trim();
            let mut new: Value = serde_json::from_str(text.trim_start_matches('+')).map_err(|_| {
                Error::Params(format!("'{text}' is not valid JSON; use a number, true/false, null, or an array (object keys and strings need double quotes)"))
            })?;
            let keys: Vec<&str> = path.split('.').collect();
            let mut node = &mut root;
            for key in &keys[..keys.len() - 1] {
                node = node.get_mut(*key).ok_or_else(|| unknown(path))?;
            }
            let slot =
                node.as_object_mut().and_then(|o| o.get_mut(keys[keys.len() - 1])).ok_or_else(|| unknown(path))?;
            if slot.is_object() {
                return Err(Error::Params(format!(
                    "'{path}' is a group; set one of its fields, e.g. {path}.{}",
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
            {"path": "gradients", "default": [], "description": "Up to 4 graduated filters, e.g. [{\"from\":[0.5,0],\"to\":[0.5,0.55],\"exposure\":-0.7,\"saturation\":15}] darkens and enriches a sky: full effect at and beyond `from`, fading to none at `to`; points are [x, y] fractions of the final picture from the top left. exposure is in stops (-3..3), saturation -100..100."},
            {"path": "lens_corrections", "default": true, "description": "Apply the camera's embedded distortion, vignetting and chromatic aberration corrections."},
        ],
        "hsl_bands": HSL_BANDS,
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
