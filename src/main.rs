use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;

use clap::{Args, Parser, Subcommand};
use serde_json::{json, Value};

use rawsmith::color::Spaces;
use rawsmith::error::{io_at, Error, Result};
use rawsmith::params::{self, Params};
use rawsmith::pipeline::{self, Size};
use rawsmith::{arw, compare, output, stats};

/// Develop RAW photos from a JSON recipe. Successful subcommands print JSON on stdout.
#[derive(Parser)]
#[command(
    name = "rawsmith",
    version,
    about,
    after_help = "Errors are JSON on stderr (exit status 1, empty stdout).\nRAWs are never written; outputs are replaced, so use a new output name to keep a version.\nValidation and staging failures preserve recipes and outputs; paired saves roll back commit failures unless interrupted or rollback fails."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Describe a RAW file: compression, camera settings, white balance and sensor exposure
    Inspect { raw: PathBuf },
    /// List every recipe parameter with its range, default and meaning
    Schema,
    /// Render a picture from a RAW file and its recipe, and report statistics of the result
    Render(Render),
    /// Change recipe parameters, save the recipe, and optionally render a preview
    Edit(Edit),
    /// Analyse the scene, save an automatic starting recipe, and optionally render a preview
    Auto(Auto),
    /// Compare upright camera and recipe versions in one unlabelled image row
    #[command(
        after_help = "With no --panel options, shows camera then saved. A single --panel is allowed; camera alone uses neutral framing. The camera uses the first non-camera panel's crop and rotation; other panels keep their own framing. Recipes are only read. Camera colour uses embedded JPEG declarations first, then RAW EXIF. Declared Adobe RGB (1998) is converted to sRGB; established sRGB rows are tagged. Unknown, conflicting or unverified camera colour produces a warning and an untagged row. Camera panels report color_declaration (source, color_space) and color_conversion."
    )]
    Compare(Compare),
    /// Copy the embedded JPEG unchanged, including metadata; report RAW orientation and needed rotation
    #[command(
        after_help = "The stored JPEG pixels are not rotated. raw_orientation reports the RAW's EXIF orientation; needs_rotation and rotation_degrees_clockwise describe the rotation of stored pixels for orientations 1, 3, 6 and 8 (null for others). A viewer may already apply embedded orientation metadata; do not rotate twice."
    )]
    ExtractJpeg {
        raw: PathBuf,
        /// Output JPEG; output_replaced reports whether a file was replaced.
        #[arg(short, long)]
        output: PathBuf,
    },
}

#[derive(Args)]
struct Source {
    /// The RAW file
    raw: PathBuf,
    /// Recipe file [default: <raw>.rawsmith.json next to the RAW]. Render reports recipe_file=null when defaults are used without a sidecar.
    #[arg(long)]
    recipe: Option<PathBuf>,
    /// Parameter assignment (repeatable): exposure=0.5 or hsl.blue.saturation=-20 sets a
    /// value, exposure+=0.3 or contrast-=10 changes the current one
    #[arg(long = "set", value_name = "PATH=VALUE", allow_hyphen_values = true)]
    set: Vec<String>,
}

#[derive(Args)]
struct Target {
    /// Output image (sRGB ICC JPEG or sRGB 16-bit PNG, reported as output_color_space); output_replaced reports whether a file was replaced.
    #[arg(short, long)]
    output: Option<PathBuf>,
    /// Longest edge in pixels (at least 1) [default: full resolution]. Small sizes render much faster.
    #[arg(long)]
    size: Option<usize>,
    /// Render only this part of the picture at full resolution, to check fine
    /// detail: x0,y0,x1,y1 as fractions of the picture
    #[arg(long, value_name = "X0,Y0,X1,Y1", conflicts_with = "size")]
    region: Option<String>,
    /// JPEG quality (1..100)
    #[arg(long, default_value_t = 92)]
    quality: u8,
}

#[derive(Args)]
struct Render {
    #[command(flatten)]
    source: Source,
    #[command(flatten)]
    target: Target,
}

#[derive(Args)]
struct Edit {
    #[command(flatten)]
    source: Source,
    #[command(flatten)]
    target: Target,
    /// Start again from the neutral default recipe before applying --set
    #[arg(long)]
    reset: bool,
}

#[derive(Args)]
struct Auto {
    #[command(flatten)]
    source: Source,
    #[command(flatten)]
    target: Target,
}

#[derive(Args)]
struct Compare {
    /// The RAW file
    raw: PathBuf,
    /// Output JPEG or 16-bit PNG (output_color_space: sRGB or mixed-unmanaged); output_replaced reports whether a file was replaced.
    #[arg(short, long)]
    output: PathBuf,
    /// Panel, repeat in left-to-right order: camera, saved, default, or recipe:FILE
    #[arg(long, value_name = "VERSION")]
    panel: Vec<String>,
    /// Bound each panel's longest edge; panels shrink to one shared height (at least 1)
    #[arg(long, default_value_t = 1400)]
    size: usize,
}

impl Target {
    fn validate(&self) -> Result<()> {
        if self.size == Some(0) {
            return Err(Error::Params("--size must be at least 1 pixel".into()));
        }
        if !(1..=100).contains(&self.quality) {
            return Err(Error::Params("--quality must be in 1..100".into()));
        }
        if let Some(region) = &self.region {
            parse_region(region)?;
        }
        Ok(())
    }
}

fn recipe_path(source: &Source) -> Result<PathBuf> {
    let path = source.recipe.clone().unwrap_or_else(|| {
        let mut name = source.raw.clone().into_os_string();
        name.push(".rawsmith.json");
        PathBuf::from(name)
    });
    protect_raw(&source.raw, &path, "recipe")?;
    Ok(path)
}

fn load_recipe(path: &Path, allow_missing: bool) -> Result<(Params, bool)> {
    match std::fs::read_to_string(path) {
        Ok(text) => {
            let mut value: Value = serde_json::from_str(&text)
                .map_err(|e| Error::Params(format!("{} is not valid JSON: {e}", path.display())))?;
            if let Some(object) = value.as_object_mut() {
                object.remove("rawsmith");
            }
            Params::from_value(value).map(|params| (params, true))
        }
        Err(e) if allow_missing && e.kind() == std::io::ErrorKind::NotFound => Ok((Params::default(), false)),
        Err(e) => Err(Error::Io(io_at("read recipe", path, e))),
    }
}

fn recipe_bytes(params: &Params) -> Vec<u8> {
    let mut value = params.to_sparse();
    value["rawsmith"] = json!(env!("CARGO_PKG_VERSION"));
    let text = serde_json::to_string_pretty(&value).expect("recipes always serialise");
    (text + "\n").into_bytes()
}

fn read_file(path: &Path) -> Result<Vec<u8>> {
    std::fs::read(path).map_err(|e| Error::Io(io_at("read", path, e)))
}

fn read_raw(path: &Path) -> Result<arw::RawImage> {
    arw::decode(&read_file(path)?)
}

fn parse_region(text: &str) -> Result<[f32; 4]> {
    let parts: Option<Vec<f32>> = text.split(',').map(|v| v.trim().parse().ok()).collect();
    match parts.as_deref() {
        Some(&[x0, y0, x1, y1]) if 0.0 <= x0 && x0 < x1 && x1 <= 1.0 && 0.0 <= y0 && y0 < y1 && y1 <= 1.0 => {
            Ok([x0, y0, x1, y1])
        }
        _ => Err(Error::Params(
            "--region takes four numbers x0,y0,x1,y1 with 0 <= x0 < x1 <= 1 and 0 <= y0 < y1 <= 1".into(),
        )),
    }
}

/// Refuses to write `destination` when it is the RAW file itself, by any
/// name: the same path, a symlink to it, or a hard link sharing its data.
/// Where hard links cannot be recognised, `output::replace` still keeps the
/// RAW's contents intact: it replaces names, it never writes into files.
fn protect_raw(raw: &Path, destination: &Path, what: &str) -> Result<()> {
    if same_file(raw, destination) {
        return Err(Error::Params(format!(
            "{what} {} is the RAW file itself; the RAW is never written",
            destination.display()
        )));
    }
    Ok(())
}

/// Whether two paths name one file: the same path (even if nothing exists
/// there yet), a symlink to it, or a hard link sharing its data.
fn same_file(a: &Path, b: &Path) -> bool {
    // A file that does not exist yet is named by its directory, resolved.
    let resolved = |p: &Path| {
        std::fs::canonicalize(p).ok().or_else(|| {
            let full = std::path::absolute(p).ok()?;
            Some(std::fs::canonicalize(full.parent()?).ok()?.join(full.file_name()?))
        })
    };
    if resolved(a).is_some_and(|x| Some(x) == resolved(b)) {
        return true;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if let (Ok(a), Ok(b)) = (std::fs::metadata(a), std::fs::metadata(b)) {
            return a.dev() == b.dev() && a.ino() == b.ino();
        }
    }
    false
}

/// The recipe and the picture are two files; one must not replace the other.
fn distinct(recipe: &Path, target: &Target) -> Result<()> {
    match &target.output {
        Some(out) if same_file(recipe, out) => {
            Err(Error::Params(format!("output {} is also the recipe file; give them different names", out.display())))
        }
        _ => Ok(()),
    }
}

/// Prepares a render and its encoded output without changing any files.
fn prepare_render(raw: &arw::RawImage, params: &Params, target: &Target) -> Result<(Value, Option<Vec<u8>>)> {
    let started = Instant::now();
    let region = target.region.as_deref().map(parse_region).transpose()?;
    let scene = pipeline::scene(raw, params, Size { max_edge: target.size })?;
    let spaces = Spaces::new();
    let mut image = pipeline::finish(&scene.image, params, &spaces);
    if let Some([x0, y0, x1, y1]) = region {
        let (w, h) = (image.width as f32, image.height as f32);
        let (x, y) = ((x0 * w) as usize, (y0 * h) as usize);
        image = image.crop(x, y, (((x1 - x0) * w) as usize).max(1), (((y1 - y0) * h) as usize).max(1));
    }
    let bytes = target.output.as_ref().map(|out| output::encode(out, &image, target.quality)).transpose()?;
    Ok((
        json!({
            "output": target.output,
            "output_color_space": target.output.as_ref().map(|_| "sRGB"),
            "output_replaced": null,
            "recipe": params.to_sparse(),
            "white_balance": scene.white_balance,
            "sensor_clipped_pct": scene.sensor_clipped_pct,
            "stats": stats::image_stats(&image, &spaces),
            "elapsed_ms": started.elapsed().as_millis() as u64,
        }),
        bytes,
    ))
}

fn render(raw_path: &Path, params: &Params, target: &Target) -> Result<Value> {
    let started = Instant::now();
    if let Some(out) = &target.output {
        protect_raw(raw_path, out, "output")?;
    }
    let (mut report, bytes) = prepare_render(&read_raw(raw_path)?, params, target)?;
    if let (Some(out), Some(bytes)) = (&target.output, bytes) {
        report["output_replaced"] = json!(output::replace(out, &bytes)?);
    }
    report["elapsed_ms"] = json!(started.elapsed().as_millis() as u64);
    Ok(report)
}

/// Both outputs are ready before either existing file is replaced.
fn save_edit(raw: &arw::RawImage, recipe: &Path, params: &Params, target: &Target) -> Result<Value> {
    let started = Instant::now();
    let recipe_data = recipe_bytes(params);
    let mut report = if let Some(out) = &target.output {
        let (mut report, bytes) = prepare_render(raw, params, target)?;
        let bytes = bytes.expect("an output path always produces encoded bytes");
        report["output_replaced"] = json!(output::replace_pair((out, &bytes), (recipe, &recipe_data))?);
        report
    } else {
        pipeline::white_balance(raw, params)?;
        output::replace(recipe, &recipe_data)?;
        json!({"recipe": params.to_sparse(), "output_color_space": null, "output_replaced": null})
    };
    if target.output.is_some() {
        report["elapsed_ms"] = json!(started.elapsed().as_millis() as u64);
    }
    report["recipe_file"] = json!(recipe);
    Ok(report)
}

fn run(cli: Cli) -> Result<Value> {
    match cli.command {
        Command::Inspect { raw: path } => {
            let raw = read_raw(&path)?;
            let (white_balance, _) = pipeline::white_balance(&raw, &Params::default())?;
            Ok(json!({
                "file": path,
                "camera": raw.meta,
                "compression": raw.compression,
                "sensor": {
                    "width": raw.crop.width,
                    "height": raw.crop.height,
                    "megapixels": (raw.crop.width * raw.crop.height) as f64 / 1e6,
                },
                "white_balance": {"temperature": white_balance.as_shot_temperature, "tint": white_balance.as_shot_tint},
                "exposure": stats::raw_stats(&raw),
                "lens_corrections_available": raw.lens.is_some(),
                "embedded_jpeg": raw.embedded_jpeg.is_some(),
            }))
        }
        Command::Schema => Ok(params::schema()),
        Command::Compare(args) => compare_versions(args),
        Command::Render(Render { source, target }) => {
            target.validate()?;
            let path = recipe_path(&source)?;
            distinct(&path, &target)?;
            let (params, loaded) = load_recipe(&path, source.recipe.is_none())?;
            let params = params.with_assignments(&source.set)?;
            let mut report = render(&source.raw, &params, &target)?;
            report["recipe_file"] = if loaded { json!(path) } else { Value::Null };
            Ok(report)
        }
        Command::Edit(Edit { source, target, reset }) => {
            target.validate()?;
            let path = recipe_path(&source)?;
            distinct(&path, &target)?;
            if let Some(out) = &target.output {
                protect_raw(&source.raw, out, "output")?;
            }
            let base = if reset { Params::default() } else { load_recipe(&path, true)?.0 };
            let params = base.with_assignments(&source.set)?;
            save_edit(&read_raw(&source.raw)?, &path, &params, &target)
        }
        Command::Auto(Auto { source, target }) => {
            target.validate()?;
            let path = recipe_path(&source)?;
            distinct(&path, &target)?;
            if let Some(out) = &target.output {
                protect_raw(&source.raw, out, "output")?;
            }
            // White balance, crop and other non-tone settings of an existing recipe are kept.
            let base = load_recipe(&path, true)?.0;
            let raw = read_raw(&source.raw)?;
            let proposal = rawsmith::auto::auto(&raw, &base)?;
            let params = proposal.params.with_assignments(&source.set)?;
            let mut report = save_edit(&raw, &path, &params, &target)?;
            report["decisions"] = json!(proposal.notes);
            Ok(report)
        }
        Command::ExtractJpeg { raw: path, output } => {
            protect_raw(&path, &output, "output")?;
            let file = read_file(&path)?;
            let raw = arw::decode(&file)?;
            let (offset, len) =
                raw.embedded_jpeg.ok_or_else(|| Error::Unsupported("this file has no embedded JPEG".into()))?;
            let replaced = output::replace(&output, &file[offset..offset + len])?;
            let rotation: Option<u16> = match raw.meta.orientation {
                1 => Some(0),
                3 => Some(180),
                6 => Some(90),
                8 => Some(270),
                _ => None,
            };
            Ok(json!({
                "output": output, "bytes": len, "output_replaced": replaced,
                "raw_orientation": raw.meta.orientation,
                "needs_rotation": rotation.map(|degrees| degrees != 0),
                "rotation_degrees_clockwise": rotation,
            }))
        }
    }
}

fn compare_versions(args: Compare) -> Result<Value> {
    if args.size == 0 {
        return Err(Error::Params("--size must be at least 1 pixel".into()));
    }
    protect_raw(&args.raw, &args.output, "output")?;
    let saved = recipe_path(&Source { raw: args.raw.clone(), recipe: None, set: vec![] })?;
    // Protect the sidecar even when it isn't selected (or doesn't exist yet).
    if same_file(&saved, &args.output) {
        return Err(Error::Params("output is also the saved recipe file; give them different names".into()));
    }
    let versions = if args.panel.is_empty() { vec!["camera".into(), "saved".into()] } else { args.panel };
    let mut panels = Vec::new();
    for version in versions {
        let (kind, params, recipe_file) = match version.as_str() {
            "camera" => ("camera", Params::default(), None),
            "default" => ("default", Params::default(), None),
            "saved" => {
                let (params, loaded) = load_recipe(&saved, true)?;
                ("saved", params, loaded.then(|| saved.clone()))
            }
            _ => {
                let path = version.strip_prefix("recipe:").filter(|s| !s.is_empty()).ok_or_else(|| {
                    Error::Params(format!("unknown panel '{version}'; use camera, saved, default or recipe:FILE"))
                })?;
                let path = PathBuf::from(path);
                protect_raw(&args.raw, &path, "recipe")?;
                if same_file(&path, &args.output) {
                    return Err(Error::Params(format!("output {} is also a recipe file", args.output.display())));
                }
                let params = load_recipe(&path, false)?.0;
                ("recipe", params, Some(path))
            }
        };
        panels.push(compare::Panel { kind, params, recipe_file });
    }
    let file = read_file(&args.raw)?;
    let raw = arw::decode(&file)?;
    let (image, mut report, srgb) = compare::row(&raw, &file, &panels, args.size)?;
    let bytes = output::encode_with_color_space(&args.output, &image, 92, srgb)?;
    report["output_replaced"] = json!(output::replace(&args.output, &bytes)?);
    report["output"] = json!(args.output);
    report["output_color_space"] = json!(if srgb { "sRGB" } else { "mixed-unmanaged" });
    Ok(report)
}

fn fail(kind: &str, message: &str) -> ExitCode {
    eprintln!("{}", json!({"error": {"kind": kind, "message": message}}));
    ExitCode::FAILURE
}

fn main() -> ExitCode {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        // --help and --version are answers, not mistakes.
        Err(e) if !e.use_stderr() => {
            print!("{e}");
            return ExitCode::SUCCESS;
        }
        Err(e) => return fail("usage", e.to_string().trim()),
    };
    match run(cli) {
        Ok(value) => {
            println!("{}", serde_json::to_string_pretty(&value).expect("reports always serialise"));
            ExitCode::SUCCESS
        }
        Err(e) => fail(e.kind(), &e.to_string()),
    }
}
