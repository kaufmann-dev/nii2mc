use crate::error::{AppError, Result};
use crate::manifest::{MINECRAFT_DATA_VERSION, MINECRAFT_VERSION};
use crate::nifti::read_nifti;
use crate::world::{self, VerticalAxis};
use clap::{CommandFactory, Parser, Subcommand};
use serde::Serialize;
use serde_json::{Value, json};
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(
    name = "nii2mc",
    version,
    about = "Round-trip discrete NIfTI label maps through editable Minecraft Java worlds",
    long_about = "Convert 3D integer .nii or .nii.gz label maps into Minecraft Java 26.2 worlds with one voxel per block, then export edited worlds back to NIfTI without changing the original header, extensions, affine, or voxel dimensions."
)]
struct Cli {
    /// Emit a stable JSON envelope to stdout; progress remains on stderr
    #[arg(long, global = true)]
    json: bool,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Report supported formats and the installed executable
    Doctor,

    /// Inspect a NIfTI label map or an nii2mc Minecraft world
    Inspect {
        /// Input .nii/.nii.gz file or Minecraft world directory
        input: PathBuf,
    },

    /// Convert a NIfTI label map into a new Minecraft Java 26.2 world
    ToWorld {
        /// Input 3D integer .nii or .nii.gz label map
        input: PathBuf,

        /// New world directory; existing paths are never overwritten
        #[arg(long)]
        output: PathBuf,

        /// NIfTI axis mapped to Minecraft's vertical Y axis
        #[arg(long, value_enum, default_value_t = VerticalAxis::Z)]
        vertical_axis: VerticalAxis,
    },

    /// Show label IDs, block IDs, embedded names, counts, and legend positions
    Palette {
        /// Minecraft world created by nii2mc
        world: PathBuf,
    },

    /// Check world metadata, required chunks, and every block in the export volume
    Validate {
        /// Minecraft world created by nii2mc; it must be closed
        world: PathBuf,
    },

    /// Export an edited nii2mc Minecraft world back to NIfTI
    ToNifti {
        /// Minecraft world created by nii2mc; it must be closed
        world: PathBuf,

        /// New .nii or .nii.gz file; existing paths are never overwritten
        #[arg(long)]
        output: PathBuf,
    },
}

#[derive(Debug, Serialize)]
struct SuccessEnvelope {
    ok: bool,
    command: &'static str,
    data: Value,
}

pub fn main_entry() {
    let requested_json = std::env::args().any(|argument| argument == "--json");
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(error) => {
            use clap::error::ErrorKind;
            if matches!(
                error.kind(),
                ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
            ) {
                let _ = error.print();
                return;
            }
            let app_error = AppError::usage(error.to_string());
            emit_error(&app_error, requested_json);
            std::process::exit(app_error.kind.exit_code());
        }
    };
    match execute(cli) {
        Ok(()) => {}
        Err((error, json_mode)) => {
            emit_error(&error, json_mode);
            std::process::exit(error.kind.exit_code());
        }
    }
}

fn execute(cli: Cli) -> std::result::Result<(), (AppError, bool)> {
    let json_mode = cli.json;
    let result = match cli.command {
        Commands::Doctor => doctor().map(|data| {
            emit_success("doctor", data, doctor_text(), json_mode);
        }),
        Commands::Inspect { input } => inspect(input).map(|(data, text)| {
            emit_success("inspect", data, text, json_mode);
        }),
        Commands::ToWorld {
            input,
            output,
            vertical_axis,
        } => world::create_world(&input, &output, vertical_axis).and_then(|report| {
            let text = format!(
                "Created Minecraft Java {} world {}\n{} voxels, {} nonzero labels; volume bounds {:?} to {:?}",
                report.minecraft_version,
                report.output.display(),
                report.voxel_count,
                report.nonzero_labels,
                report.volume_bounds.min,
                report.volume_bounds.max
            );
            emit_serializable("to-world", &report, text, json_mode)
        }),
        Commands::Palette { world: path } => world::palette(&path).and_then(|palette| {
            let mut text = format!("{} label mappings in {}", palette.len(), path.display());
            for entry in &palette {
                text.push_str(&format!(
                    "\n{}\t{}\t{}\t{} voxels\tlegend {:?}",
                    entry.label,
                    entry.block,
                    entry.name.as_deref().unwrap_or("unnamed"),
                    entry.voxel_count,
                    entry.legend_position
                ));
            }
            emit_serializable("palette", &palette, text, json_mode)
        }),
        Commands::Validate { world: path } => world::validate_world(&path).and_then(|report| {
            let text = format!(
                "Valid nii2mc world: {} chunks and {} voxels checked; {} labels; Minecraft Java {}",
                report.required_chunks,
                report.checked_voxels,
                report.labels,
                report.minecraft_version
            );
            emit_serializable("validate", &report, text, json_mode)
        }),
        Commands::ToNifti { world: path, output } => {
            world::export_nifti(&path, &output).and_then(|report| {
                let text = format!(
                    "Created {} with {} voxels and the preserved NIfTI header/extensions",
                    report.output.display(), report.voxel_count
                );
                emit_serializable("to-nifti", &report, text, json_mode)
            })
        }
    };
    result.map_err(|error| (error, json_mode))
}

fn doctor() -> Result<Value> {
    let executable = std::env::current_exe()
        .map_err(|error| AppError::io(format!("cannot resolve current executable: {error}")))?;
    Ok(json!({
        "version": env!("CARGO_PKG_VERSION"),
        "executable": executable,
        "minecraft": {
            "edition": "Java",
            "version": MINECRAFT_VERSION,
            "data_version": MINECRAFT_DATA_VERSION
        },
        "nifti": {
            "container": "NIfTI-1 single-file",
            "extensions": [".nii", ".nii.gz"],
            "dimensions": 3,
            "datatypes": ["uint8", "int8", "uint16", "int16", "uint32", "int32"],
            "maximum_nonzero_labels": 127
        },
        "network_required": false
    }))
}

fn doctor_text() -> String {
    format!(
        "nii2mc {} is ready\nMinecraft: Java {} (data version {})\nNIfTI: 3D single-file integer label maps (.nii and .nii.gz)\nMaximum nonzero labels: 127\nNetwork: not required",
        env!("CARGO_PKG_VERSION"),
        MINECRAFT_VERSION,
        MINECRAFT_DATA_VERSION
    )
}

fn inspect(input: PathBuf) -> Result<(Value, String)> {
    if input.is_dir() {
        let inspection = world::inspect_world(&input)?;
        let text = format!(
            "nii2mc Minecraft Java {} world\nSource: {}\nDimensions: {:?}\nSpacing: {:?}\nVertical NIfTI axis: {}\nLabels: {}",
            inspection.minecraft_version,
            inspection.source_filename,
            inspection.dimensions,
            inspection.spacing,
            inspection.vertical_axis,
            inspection.labels.len()
        );
        let value = serde_json::to_value(inspection)
            .map_err(|error| AppError::io(format!("cannot serialize inspection: {error}")))?;
        Ok((value, text))
    } else {
        let volume = read_nifti(&input)?;
        let inspection = volume.inspection();
        let text = format!(
            "NIfTI label map\nDimensions: {:?}\nSpacing: {:?}\nDatatype: {} ({})\nVoxels: {}\nNonzero labels: {}\nEmbedded label names: {}",
            inspection.dimensions,
            inspection.spacing,
            inspection.datatype,
            inspection.endianness,
            inspection.voxel_count,
            inspection.nonzero_labels,
            if inspection.embedded_label_names {
                "yes"
            } else {
                "no"
            }
        );
        let value = serde_json::to_value(inspection)
            .map_err(|error| AppError::io(format!("cannot serialize inspection: {error}")))?;
        Ok((value, text))
    }
}

fn emit_serializable<T: Serialize>(
    command: &'static str,
    value: &T,
    text: String,
    json_mode: bool,
) -> Result<()> {
    let data = serde_json::to_value(value)
        .map_err(|error| AppError::io(format!("cannot serialize command result: {error}")))?;
    emit_success(command, data, text, json_mode);
    Ok(())
}

fn emit_success(command: &'static str, data: Value, text: String, json_mode: bool) {
    if json_mode {
        let envelope = SuccessEnvelope {
            ok: true,
            command,
            data,
        };
        println!(
            "{}",
            serde_json::to_string_pretty(&envelope).expect("success envelope is serializable")
        );
    } else {
        println!("{text}");
    }
}

fn emit_error(error: &AppError, json_mode: bool) {
    if json_mode {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "ok": false,
                "error": {
                    "code": error.kind.code(),
                    "message": error.message,
                    "details": error.details
                }
            }))
            .expect("error envelope is serializable")
        );
    } else {
        eprintln!("error: {}", error.message);
        if let Some(details) = &error.details {
            eprintln!(
                "{}",
                serde_json::to_string_pretty(details).unwrap_or_else(|_| details.to_string())
            );
        }
        eprintln!("Run 'nii2mc --help' for usage.");
    }
}

#[allow(dead_code)]
fn help_text() -> String {
    Cli::command().render_long_help().to_string()
}
