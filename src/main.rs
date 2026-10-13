use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;

use clap::{Args, Parser, Subcommand};
use serde_json::{json, Value};

use rawsmith::color::Spaces;
use rawsmith::error::{Error, Result};
use rawsmith::params::{self, Params};
use rawsmith::pipeline::{self, Size};
use rawsmith::{arw, output, stats};

/// Develop RAW photos from a JSON recipe. Every command prints JSON on stdout.
#[derive(Parser)]
#[command(name = "rawsmith", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Describe a RAW file: camera settings, white balance and exposure of the sensor data
    Inspect { raw: PathBuf },
    /// List every recipe parameter with its range, default and meaning
    Schema,
    /// Render a picture from a RAW file and its recipe, and report statistics of the result
    Render(Render),
    /// Change recipe parameters, save the recipe, and optionally render a preview
    Edit(Edit),
    /// Analyse the scene, save an automatic starting recipe, and optionally render a preview
    Auto(Auto),
    /// Save the camera's own embedded JPEG rendering, for reference
    ExtractJpeg {
        raw: PathBuf,
        #[arg(short, long)]
        output: PathBuf,
    },
}

#[derive(Args)]
struct Source {
    /// The RAW file. It is only ever read.
    raw: PathBuf,
    /// Recipe file [default: <raw>.rawsmith.json next to the RAW]
    #[arg(long)]
    recipe: Option<PathBuf>,
    /// Parameter assignment (repeatable): exposure=0.5 or hsl.blue.saturation=-20 sets a
    /// value, exposure+=0.3 or contrast-=10 changes the current one
    #[arg(long = "set", value_name = "PATH=VALUE", allow_hyphen_values = true)]
    set: Vec<String>,
}

#[derive(Args)]
struct Target {
    /// Output image (.jpg or 16-bit .png)
    #[arg(short, long)]
    output: Option<PathBuf>,
    /// Longest edge in pixels [default: full resolution]. Small sizes render much faster.
    #[arg(long)]
    size: Option<usize>,
    /// Render only this part of the picture at full resolution, to check fine
    /// detail: x0,y0,x1,y1 as fractions of the picture
    #[arg(long, value_name = "X0,Y0,X1,Y1", conflicts_with = "size")]
    region: Option<String>,
    /// JPEG quality
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

fn recipe_path(source: &Source) -> Result<PathBuf> {
    let path = source.recipe.clone().unwrap_or_else(|| {
        let mut name = source.raw.clone().into_os_string();
        name.push(".rawsmith.json");
        PathBuf::from(name)
    });
    protect_raw(&source.raw, &path, "recipe")?;
    Ok(path)
}

fn load_recipe(path: &Path) -> Result<Params> {
    match std::fs::read_to_string(path) {
        Ok(text) => {
            let mut value: Value = serde_json::from_str(&text)
                .map_err(|e| Error::Params(format!("{} is not valid JSON: {e}", path.display())))?;
            if let Some(object) = value.as_object_mut() {
                object.remove("rawsmith");
            }
            Params::from_value(value)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Params::default()),
        Err(e) => Err(e.into()),
    }
}

fn save_recipe(path: &Path, params: &Params) -> Result<()> {
    let mut value = params.to_sparse();
    value["rawsmith"] = json!(env!("CARGO_PKG_VERSION"));
    let text = serde_json::to_string_pretty(&value).expect("recipes always serialise");
    // Write-then-rename so an interrupted save never leaves half a recipe.
    // The staging file must be new: `create_new` refuses an existing name
    // without following it, so a link planted there cannot redirect the write.
    let mut name = path.as_os_str().to_owned();
    name.push(format!(".{}.tmp", std::process::id()));
    let temporary = PathBuf::from(name);
    let written = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .and_then(|mut file| file.write_all(text.as_bytes()).and_then(|()| file.write_all(b"\n")))
        .and_then(|()| std::fs::rename(&temporary, path));
    if written.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    Ok(written?)
}

fn read_raw(path: &Path) -> Result<arw::RawImage> {
    let file = std::fs::read(path)
        .map_err(|e| Error::Io(std::io::Error::new(e.kind(), format!("cannot read {}: {e}", path.display()))))?;
    arw::decode(&file)
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
    let resolved = |p: &Path| std::fs::canonicalize(p).or_else(|_| std::path::absolute(p)).ok();
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

/// Renders `params` and writes the output; returns the JSON report.
fn render(raw_path: &Path, params: &Params, target: &Target) -> Result<Value> {
    let started = Instant::now();
    if let Some(out) = &target.output {
        protect_raw(raw_path, out, "output")?;
    }
    let region = target.region.as_deref().map(parse_region).transpose()?;
    let raw = read_raw(raw_path)?;
    let scene = pipeline::scene(&raw, params, Size { max_edge: target.size })?;
    let spaces = Spaces::new();
    let mut image = pipeline::finish(&scene.image, params, &spaces);
    if let Some([x0, y0, x1, y1]) = region {
        let (w, h) = (image.width as f32, image.height as f32);
        let (x, y) = ((x0 * w) as usize, (y0 * h) as usize);
        image = image.crop(x, y, (((x1 - x0) * w) as usize).max(1), (((y1 - y0) * h) as usize).max(1));
    }
    if let Some(out) = &target.output {
        output::write(out, &image, target.quality)?;
    }
    Ok(json!({
        "output": target.output,
        "recipe": params.to_sparse(),
        "white_balance": scene.white_balance,
        "sensor_clipped_pct": scene.sensor_clipped_pct,
        "stats": stats::image_stats(&image, &spaces),
        "elapsed_ms": started.elapsed().as_millis() as u64,
    }))
}

fn run(cli: Cli) -> Result<Value> {
    match cli.command {
        Command::Inspect { raw: path } => {
            let raw = read_raw(&path)?;
            let (white_balance, _) = pipeline::white_balance(&raw, &Params::default())?;
            Ok(json!({
                "file": path,
                "camera": raw.meta,
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
        Command::Render(Render { source, target }) => {
            let path = recipe_path(&source)?;
            distinct(&path, &target)?;
            let params = load_recipe(&path)?.with_assignments(&source.set)?;
            render(&source.raw, &params, &target)
        }
        Command::Edit(Edit { source, target, reset }) => {
            let path = recipe_path(&source)?;
            distinct(&path, &target)?;
            let base = if reset { Params::default() } else { load_recipe(&path)? };
            let params = base.with_assignments(&source.set)?;
            // Render first: a recipe that cannot be rendered is not saved.
            let mut report = if target.output.is_some() {
                render(&source.raw, &params, &target)?
            } else {
                json!({"recipe": params.to_sparse()})
            };
            save_recipe(&path, &params)?;
            report["recipe_file"] = json!(path);
            Ok(report)
        }
        Command::Auto(Auto { source, target }) => {
            let path = recipe_path(&source)?;
            distinct(&path, &target)?;
            // White balance, crop and other non-tone settings of an existing recipe are kept.
            let base = load_recipe(&path)?;
            let proposal = rawsmith::auto::auto(&read_raw(&source.raw)?, &base)?;
            let params = proposal.params.with_assignments(&source.set)?;
            let mut report = if target.output.is_some() {
                render(&source.raw, &params, &target)?
            } else {
                json!({"recipe": params.to_sparse()})
            };
            save_recipe(&path, &params)?;
            report["recipe_file"] = json!(path);
            report["decisions"] = json!(proposal.notes);
            Ok(report)
        }
        Command::ExtractJpeg { raw: path, output } => {
            protect_raw(&path, &output, "output")?;
            let file = std::fs::read(&path)?;
            let raw = arw::decode(&file)?;
            let (offset, len) =
                raw.embedded_jpeg.ok_or_else(|| Error::Unsupported("this file has no embedded JPEG".into()))?;
            std::fs::write(&output, &file[offset..offset + len])?;
            Ok(json!({"output": output, "bytes": len}))
        }
    }
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
