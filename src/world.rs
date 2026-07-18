use crate::anvil::{self, MAX_Y, MIN_Y, compound};
use crate::error::{AppError, Result};
use crate::manifest::{
    AxisMapping, ChunkBounds, MINECRAFT_DATA_VERSION, MINECRAFT_VERSION, Manifest, NiftiMetadata,
    PaletteEntry, SCHEMA_VERSION, WorldBounds,
};
use crate::nifti::{NiftiVolume, read_prefix, sha256_bytes, write_nifti};
use crate::palette::assign_palette;
use clap::ValueEnum;
use fastanvil::{Chunk, CurrentJavaChunk};
use fastnbt::Value;
use fs2::FileExt;
use serde::Serialize;
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use tempfile::TempDir;

type BlockPosition = (i32, i32, i32);
type GuideBlocks = HashMap<BlockPosition, String>;

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum VerticalAxis {
    X,
    Y,
    Z,
}

impl VerticalAxis {
    fn index(self) -> usize {
        match self {
            Self::X => 0,
            Self::Y => 1,
            Self::Z => 2,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::X => "x",
            Self::Y => "y",
            Self::Z => "z",
        }
    }
}

#[derive(Debug, Serialize)]
pub struct WorldInspection {
    pub kind: &'static str,
    pub minecraft_version: String,
    pub minecraft_data_version: i32,
    pub source_filename: String,
    pub source_sha256: String,
    pub dimensions: [u32; 3],
    pub spacing: [f32; 3],
    pub vertical_axis: String,
    pub volume_bounds: WorldBounds,
    pub labels: Vec<PaletteEntry>,
}

#[derive(Debug, Serialize)]
pub struct ConversionReport {
    pub output: PathBuf,
    pub dimensions: [u32; 3],
    pub voxel_count: u64,
    pub nonzero_labels: usize,
    pub minecraft_version: String,
    pub volume_bounds: WorldBounds,
}

#[derive(Debug, Serialize)]
pub struct ValidationReport {
    pub valid: bool,
    pub required_chunks: usize,
    pub checked_voxels: u64,
    pub labels: usize,
    pub minecraft_version: String,
}

#[derive(Debug, Clone)]
struct Placement {
    vertical: usize,
    horizontal: [usize; 2],
    bounds: WorldBounds,
}

impl Placement {
    fn new(dimensions: [u32; 3], vertical_axis: VerticalAxis) -> Result<Self> {
        let vertical = vertical_axis.index();
        let vertical_length = dimensions[vertical] as i32;
        if vertical_length > MAX_Y - MIN_Y + 1 {
            let fitting: Vec<&str> = [VerticalAxis::X, VerticalAxis::Y, VerticalAxis::Z]
                .into_iter()
                .filter(|axis| dimensions[axis.index()] as i32 <= MAX_Y - MIN_Y + 1)
                .map(VerticalAxis::name)
                .collect();
            return Err(AppError::incompatible(format!(
                "NIfTI {} axis has {} voxels, exceeding Minecraft's 384-block build height; fitting axes: {}",
                vertical_axis.name(),
                vertical_length,
                if fitting.is_empty() { "none".to_string() } else { fitting.join(", ") }
            ))
            .with_details(json!({"vertical_length": vertical_length, "fitting_axes": fitting})));
        }
        let horizontal: Vec<usize> = (0..3).filter(|axis| *axis != vertical).collect();
        let horizontal = [horizontal[0], horizontal[1]];
        let len_x = dimensions[horizontal[0]] as i32;
        let len_z = dimensions[horizontal[1]] as i32;
        let len_y = dimensions[vertical] as i32;
        let min_x = -(len_x / 2);
        let min_z = -(len_z / 2);
        let min_y = MIN_Y + ((MAX_Y - MIN_Y + 1 - len_y) / 2);
        Ok(Self {
            vertical,
            horizontal,
            bounds: WorldBounds {
                min: [min_x, min_y, min_z],
                max: [min_x + len_x - 1, min_y + len_y - 1, min_z + len_z - 1],
            },
        })
    }

    fn axis_mapping(&self) -> AxisMapping {
        let minecraft_axis = |nifti_axis: usize| {
            if self.vertical == nifti_axis {
                "y"
            } else if self.horizontal[0] == nifti_axis {
                "x"
            } else {
                "z"
            }
            .to_string()
        };
        AxisMapping {
            nifti_x_to_minecraft: minecraft_axis(0),
            nifti_y_to_minecraft: minecraft_axis(1),
            nifti_z_to_minecraft: minecraft_axis(2),
            vertical_axis: ["x", "y", "z"][self.vertical].to_string(),
        }
    }

    fn voxel_index(&self, world: [i32; 3], dimensions: [u32; 3]) -> Option<usize> {
        if (0..3)
            .any(|axis| world[axis] < self.bounds.min[axis] || world[axis] > self.bounds.max[axis])
        {
            return None;
        }
        let mut nifti = [0usize; 3];
        nifti[self.horizontal[0]] = (world[0] - self.bounds.min[0]) as usize;
        nifti[self.vertical] = (world[1] - self.bounds.min[1]) as usize;
        nifti[self.horizontal[1]] = (world[2] - self.bounds.min[2]) as usize;
        Some(nifti[0] + dimensions[0] as usize * (nifti[1] + dimensions[1] as usize * nifti[2]))
    }
}

pub fn create_world(
    input: &Path,
    output: &Path,
    vertical_axis: VerticalAxis,
) -> Result<ConversionReport> {
    if output.exists() {
        return Err(AppError::usage(format!(
            "output {} already exists; refusing to overwrite it",
            output.display()
        )));
    }
    eprintln!("Reading and validating {}...", input.display());
    let volume = crate::nifti::read_nifti(input)?;
    let placement = Placement::new(volume.metadata.dimensions, vertical_axis)?;
    let mut palette = assign_palette(&volume.counts, &volume.label_names)?;
    let (guides, spawn) = build_guides(&placement.bounds, &mut palette);
    let chunk_bounds = chunk_bounds(&placement.bounds);
    let manifest = make_manifest(
        input,
        &volume,
        &placement,
        chunk_bounds.clone(),
        palette.clone(),
    );

    let parent = output.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let temporary = TempDir::new_in(parent)?;
    let staging = temporary.path().join(
        output
            .file_name()
            .ok_or_else(|| AppError::usage("output must name a world directory"))?,
    );
    fs::create_dir(&staging)?;
    write_world_metadata(&staging, output, &placement.bounds, &guides, spawn)?;
    manifest.write(&staging)?;
    fs::write(Manifest::prefix_path(&staging), &volume.prefix)?;

    let mut chunks = required_chunk_set(chunk_bounds);
    for position in guides.keys() {
        chunks.insert((position.0.div_euclid(16), position.2.div_euclid(16)));
    }
    let label_blocks: HashMap<u32, String> = palette
        .iter()
        .map(|entry| (entry.label, entry.block.clone()))
        .collect();
    eprintln!(
        "Writing {} Minecraft chunks for Java {}...",
        chunks.len(),
        MINECRAFT_VERSION
    );
    anvil::write_chunks(&staging, &chunks, |x, y, z| {
        if let Some(block) = guides.get(&(x, y, z)) {
            return Some(block.clone());
        }
        let index = placement.voxel_index([x, y, z], volume.metadata.dimensions)?;
        let label = volume.voxels[index];
        label_blocks.get(&label).cloned()
    })?;
    fs::rename(&staging, output)?;
    eprintln!("Created {}", output.display());
    Ok(ConversionReport {
        output: output.to_path_buf(),
        dimensions: volume.metadata.dimensions,
        voxel_count: volume.voxels.len() as u64,
        nonzero_labels: palette.len(),
        minecraft_version: MINECRAFT_VERSION.to_string(),
        volume_bounds: placement.bounds,
    })
}

fn make_manifest(
    input: &Path,
    volume: &NiftiVolume,
    placement: &Placement,
    required_chunks: ChunkBounds,
    palette: Vec<PaletteEntry>,
) -> Manifest {
    Manifest {
        schema_version: SCHEMA_VERSION,
        created_by: format!("nii2mc {}", env!("CARGO_PKG_VERSION")),
        minecraft_version: MINECRAFT_VERSION.to_string(),
        minecraft_data_version: MINECRAFT_DATA_VERSION,
        source_filename: input
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| input.display().to_string()),
        source_sha256: volume.source_sha256.clone(),
        prefix_sha256: sha256_bytes(&volume.prefix),
        nifti: volume.metadata.clone(),
        axes: placement.axis_mapping(),
        volume_bounds: placement.bounds.clone(),
        required_chunks,
        palette,
    }
}

fn build_guides(bounds: &WorldBounds, palette: &mut [PaletteEntry]) -> (GuideBlocks, [i32; 3]) {
    let mut guides = HashMap::new();
    let frame_min_x = bounds.min[0] - 2;
    let frame_max_x = bounds.max[0] + 2;
    let frame_min_z = bounds.min[2] - 2;
    let frame_max_z = bounds.max[2] + 2;
    let frame = "minecraft:sea_lantern".to_string();
    for y in bounds.min[1]..=bounds.max[1] {
        for (x, z) in [
            (frame_min_x, frame_min_z),
            (frame_min_x, frame_max_z),
            (frame_max_x, frame_min_z),
            (frame_max_x, frame_max_z),
        ] {
            guides.insert((x, y, z), frame.clone());
        }
    }
    for x in frame_min_x..=frame_max_x {
        for y in [bounds.min[1], bounds.max[1]] {
            guides.insert((x, y, frame_min_z), frame.clone());
            guides.insert((x, y, frame_max_z), frame.clone());
        }
    }
    for z in frame_min_z..=frame_max_z {
        for y in [bounds.min[1], bounds.max[1]] {
            guides.insert((frame_min_x, y, z), frame.clone());
            guides.insert((frame_max_x, y, z), frame.clone());
        }
    }

    let origin_x = bounds.max[0] + 10;
    let origin_z = bounds.min[2];
    let platform_y = (bounds.min[1] + 5).clamp(MIN_Y + 1, MAX_Y - 3);
    let rows = palette.len().max(1).div_ceil(16) as i32;
    for x in origin_x - 3..=origin_x + 18 {
        for z in origin_z - 5..=origin_z + rows + 2 {
            guides.insert((x, platform_y, z), "minecraft:smooth_quartz".to_string());
        }
    }
    for (index, entry) in palette.iter_mut().enumerate() {
        let position = [
            origin_x + (index % 16) as i32,
            platform_y + 1,
            origin_z + (index / 16) as i32,
        ];
        entry.legend_position = position;
        guides.insert((position[0], position[1], position[2]), entry.block.clone());
    }
    let spawn = [origin_x, platform_y + 1, origin_z - 3];
    (guides, spawn)
}

fn chunk_bounds(bounds: &WorldBounds) -> ChunkBounds {
    ChunkBounds {
        min_x: bounds.min[0].div_euclid(16),
        max_x: bounds.max[0].div_euclid(16),
        min_z: bounds.min[2].div_euclid(16),
        max_z: bounds.max[2].div_euclid(16),
    }
}

fn required_chunk_set(bounds: ChunkBounds) -> BTreeSet<(i32, i32)> {
    let mut chunks = BTreeSet::new();
    for chunk_z in bounds.min_z..=bounds.max_z {
        for chunk_x in bounds.min_x..=bounds.max_x {
            chunks.insert((chunk_x, chunk_z));
        }
    }
    chunks
}

fn write_world_metadata(
    world: &Path,
    output: &Path,
    volume_bounds: &WorldBounds,
    guides: &GuideBlocks,
    spawn: [i32; 3],
) -> Result<()> {
    let name = output
        .file_name()
        .map(|value| value.to_string_lossy().into_owned())
        .unwrap_or_else(|| "nii2mc world".to_string());
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let now_ms = now.as_millis().min(i64::MAX as u128) as i64;
    let level_data = HashMap::from([(
        "Data".to_string(),
        compound([
            ("DataVersion", Value::Int(MINECRAFT_DATA_VERSION)),
            ("version", Value::Int(19133)),
            ("LevelName", Value::String(name)),
            ("LastPlayed", Value::Long(now_ms)),
            ("GameType", Value::Int(1)),
            ("allowCommands", Value::Byte(1)),
            ("initialized", Value::Byte(1)),
            ("WasModded", Value::Byte(0)),
            ("SpawnX", Value::Int(spawn[0])),
            ("SpawnY", Value::Int(spawn[1])),
            ("SpawnZ", Value::Int(spawn[2])),
            ("SpawnAngle", Value::Float(0.0)),
            (
                "Version",
                compound([
                    ("Id", Value::Int(MINECRAFT_DATA_VERSION)),
                    ("Name", Value::String(MINECRAFT_VERSION.to_string())),
                    ("Series", Value::String("main".to_string())),
                    ("Snapshot", Value::Byte(0)),
                ]),
            ),
            (
                "DataPacks",
                compound([
                    (
                        "Enabled",
                        Value::List(vec![Value::String("vanilla".to_string())]),
                    ),
                    ("Disabled", Value::List(Vec::new())),
                ]),
            ),
            (
                "enabled_features",
                Value::List(vec![Value::String("minecraft:vanilla".to_string())]),
            ),
            (
                "difficulty_settings",
                compound([
                    ("difficulty", Value::String("peaceful".to_string())),
                    ("locked", Value::Byte(1)),
                    ("hardcore", Value::Byte(0)),
                ]),
            ),
            ("server_brands", Value::List(Vec::new())),
        ]),
    )]);
    anvil::write_gzip_nbt(&world.join("level.dat"), &level_data)?;

    let data_root = |data| {
        HashMap::from([
            (
                "DataVersion".to_string(),
                Value::Int(MINECRAFT_DATA_VERSION),
            ),
            ("data".to_string(), data),
        ])
    };
    let game_rules = compound([(
        "rules",
        compound([
            ("doDaylightCycle", Value::String("false".to_string())),
            ("doWeatherCycle", Value::String("false".to_string())),
            ("doMobSpawning", Value::String("false".to_string())),
            ("doPatrolSpawning", Value::String("false".to_string())),
            ("doTraderSpawning", Value::String("false".to_string())),
            ("doWardenSpawning", Value::String("false".to_string())),
            ("doInsomnia", Value::String("false".to_string())),
            ("doFireTick", Value::String("false".to_string())),
            ("mobGriefing", Value::String("false".to_string())),
            ("randomTickSpeed", Value::String("0".to_string())),
            ("doVinesSpread", Value::String("false".to_string())),
            ("keepInventory", Value::String("true".to_string())),
        ]),
    )]);
    anvil::write_gzip_nbt(
        &world.join("data/minecraft/game_rules.dat"),
        &data_root(game_rules),
    )?;
    let clocks = compound([(
        "minecraft:overworld",
        compound([
            ("total_ticks", Value::Long(6000)),
            ("paused", Value::Byte(1)),
        ]),
    )]);
    anvil::write_gzip_nbt(
        &world.join("data/minecraft/world_clocks.dat"),
        &data_root(clocks),
    )?;
    let weather = compound([
        ("clear_weather_time", Value::Int(1_000_000)),
        ("rain_time", Value::Int(0)),
        ("raining", Value::Byte(0)),
        ("thunder_time", Value::Int(0)),
        ("thundering", Value::Byte(0)),
    ]);
    anvil::write_gzip_nbt(
        &world.join("data/minecraft/weather.dat"),
        &data_root(weather),
    )?;
    let world_gen = compound([
        ("bonus_chest", Value::Byte(0)),
        ("generate_features", Value::Byte(0)),
        ("seed", Value::Long(0)),
        (
            "dimensions",
            Value::Compound(HashMap::from([
                (
                    "minecraft:overworld".to_string(),
                    dimension_generator("minecraft:overworld", "minecraft:the_void"),
                ),
                (
                    "minecraft:the_nether".to_string(),
                    dimension_generator("minecraft:the_nether", "minecraft:nether_wastes"),
                ),
                (
                    "minecraft:the_end".to_string(),
                    dimension_generator("minecraft:the_end", "minecraft:the_end"),
                ),
            ])),
        ),
    ]);
    anvil::write_gzip_nbt(
        &world.join("data/minecraft/world_gen_settings.dat"),
        &data_root(world_gen),
    )?;

    let mut min_x = volume_bounds.min[0];
    let mut max_x = volume_bounds.max[0];
    let mut min_z = volume_bounds.min[2];
    let mut max_z = volume_bounds.max[2];
    for (x, _, z) in guides.keys() {
        min_x = min_x.min(*x);
        max_x = max_x.max(*x);
        min_z = min_z.min(*z);
        max_z = max_z.max(*z);
    }
    let border_size = (max_x - min_x + 129).max(max_z - min_z + 129) as f64;
    let border = compound([
        ("center_x", Value::Double((min_x + max_x) as f64 / 2.0)),
        ("center_z", Value::Double((min_z + max_z) as f64 / 2.0)),
        ("size", Value::Double(border_size)),
        ("size_lerp_time", Value::Long(0)),
        ("size_lerp_target", Value::Double(border_size)),
        ("safe_zone", Value::Double(5.0)),
        ("damage_per_block", Value::Double(0.0)),
        ("warning_blocks", Value::Int(5)),
        ("warning_time", Value::Int(15)),
    ]);
    anvil::write_gzip_nbt(
        &world.join("dimensions/minecraft/overworld/data/world_border.dat"),
        &data_root(border),
    )?;

    let mut lock = File::create(world.join("session.lock"))?;
    lock.write_all(&now_ms.to_be_bytes())?;
    Ok(())
}

fn dimension_generator(dimension_type: &str, biome: &str) -> Value {
    compound([
        ("type", Value::String(dimension_type.to_string())),
        (
            "generator",
            compound([
                ("type", Value::String("minecraft:flat".to_string())),
                (
                    "settings",
                    compound([
                        ("biome", Value::String(biome.to_string())),
                        ("features", Value::Byte(0)),
                        ("lakes", Value::Byte(0)),
                        (
                            "layers",
                            Value::List(vec![compound([
                                ("block", Value::String("minecraft:air".to_string())),
                                ("height", Value::Int(1)),
                            ])]),
                        ),
                        ("structure_overrides", Value::List(Vec::new())),
                    ]),
                ),
            ]),
        ),
    ])
}

pub fn inspect_world(world: &Path) -> Result<WorldInspection> {
    let manifest = Manifest::load(world)?;
    Ok(WorldInspection {
        kind: "minecraft_world",
        minecraft_version: manifest.minecraft_version,
        minecraft_data_version: manifest.minecraft_data_version,
        source_filename: manifest.source_filename,
        source_sha256: manifest.source_sha256,
        dimensions: manifest.nifti.dimensions,
        spacing: manifest.nifti.spacing,
        vertical_axis: manifest.axes.vertical_axis,
        volume_bounds: manifest.volume_bounds,
        labels: manifest.palette,
    })
}

pub fn validate_world(world: &Path) -> Result<ValidationReport> {
    ensure_world_unlocked(world)?;
    let manifest = validate_metadata(world)?;
    let voxels = decode_world(world, &manifest)?;
    Ok(ValidationReport {
        valid: true,
        required_chunks: required_chunk_set(manifest.required_chunks).len(),
        checked_voxels: voxels.len() as u64,
        labels: manifest.palette.len(),
        minecraft_version: manifest.minecraft_version,
    })
}

fn validate_metadata(world: &Path) -> Result<Manifest> {
    if !world.is_dir() {
        return Err(AppError::usage(format!(
            "{} is not a Minecraft world directory",
            world.display()
        )));
    }
    let manifest = Manifest::load(world)?;
    if !world.join("level.dat").is_file() {
        return Err(AppError::incompatible("world is missing level.dat"));
    }
    let prefix = read_prefix(&Manifest::prefix_path(world))?;
    let actual_hash = sha256_bytes(&prefix);
    if actual_hash != manifest.prefix_sha256 {
        return Err(AppError::incompatible(
            "saved NIfTI header/extension prefix does not match the manifest checksum",
        ));
    }
    let mut blocks = BTreeSet::new();
    let mut labels = BTreeSet::new();
    for entry in &manifest.palette {
        if !blocks.insert(&entry.block) {
            return Err(AppError::incompatible(format!(
                "manifest maps more than one label to {}",
                entry.block
            )));
        }
        if entry.label == 0 || !labels.insert(entry.label) {
            return Err(AppError::incompatible(format!(
                "manifest contains invalid or duplicate label {}",
                entry.label
            )));
        }
    }
    Ok(manifest)
}

pub fn export_nifti(world: &Path, output: &Path) -> Result<ConversionReport> {
    if output.exists() {
        return Err(AppError::usage(format!(
            "output {} already exists; refusing to overwrite it",
            output.display()
        )));
    }
    let output_text = output.to_string_lossy();
    if !(output_text.ends_with(".nii") || output_text.ends_with(".nii.gz")) {
        return Err(AppError::usage("output path must end in .nii or .nii.gz"));
    }
    ensure_world_unlocked(world)?;
    let manifest = validate_metadata(world)?;
    eprintln!("Validating and reading Minecraft blocks...");
    let voxels = decode_world(world, &manifest)?;
    let prefix = read_prefix(&Manifest::prefix_path(world))?;

    let parent = output.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let temporary = TempDir::new_in(parent)?;
    let staging = temporary.path().join(
        output
            .file_name()
            .ok_or_else(|| AppError::usage("output must name a NIfTI file"))?,
    );
    write_nifti(&staging, &prefix, &manifest.nifti, &voxels)?;
    fs::rename(&staging, output)?;
    eprintln!("Created {}", output.display());
    Ok(ConversionReport {
        output: output.to_path_buf(),
        dimensions: manifest.nifti.dimensions,
        voxel_count: voxels.len() as u64,
        nonzero_labels: manifest.palette.len(),
        minecraft_version: manifest.minecraft_version,
        volume_bounds: manifest.volume_bounds,
    })
}

fn decode_world(world: &Path, manifest: &Manifest) -> Result<Vec<u32>> {
    let placement = placement_from_manifest(manifest)?;
    let reverse: HashMap<&str, u32> = manifest
        .palette
        .iter()
        .map(|entry| (entry.block.as_str(), entry.label))
        .collect();
    let expected = manifest
        .nifti
        .dimensions
        .iter()
        .try_fold(1usize, |total, dimension| {
            total.checked_mul(*dimension as usize)
        })
        .ok_or_else(|| AppError::incompatible("NIfTI dimensions overflow this platform"))?;
    let mut voxels = vec![0u32; expected];
    let mut unknown_counts: BTreeMap<String, u64> = BTreeMap::new();
    let mut unknown_samples = Vec::new();
    for chunk_z in manifest.required_chunks.min_z..=manifest.required_chunks.max_z {
        for chunk_x in manifest.required_chunks.min_x..=manifest.required_chunks.max_x {
            let bytes = anvil::read_chunk(world, chunk_x, chunk_z)?.ok_or_else(|| {
                AppError::incompatible(format!("required chunk ({chunk_x}, {chunk_z}) is missing"))
            })?;
            let chunk: CurrentJavaChunk = fastnbt::from_bytes(&bytes).map_err(|error| {
                AppError::incompatible(format!(
                    "chunk ({chunk_x}, {chunk_z}) has invalid NBT: {error}"
                ))
            })?;
            if chunk.data_version != MINECRAFT_DATA_VERSION {
                return Err(AppError::incompatible(format!(
                    "chunk ({chunk_x}, {chunk_z}) has data version {}; expected {}",
                    chunk.data_version, MINECRAFT_DATA_VERSION
                )));
            }
            let min_x = manifest.volume_bounds.min[0].max(chunk_x * 16);
            let max_x = manifest.volume_bounds.max[0].min(chunk_x * 16 + 15);
            let min_z = manifest.volume_bounds.min[2].max(chunk_z * 16);
            let max_z = manifest.volume_bounds.max[2].min(chunk_z * 16 + 15);
            for world_z in min_z..=max_z {
                for world_y in manifest.volume_bounds.min[1]..=manifest.volume_bounds.max[1] {
                    for world_x in min_x..=max_x {
                        let local_x = world_x.rem_euclid(16) as usize;
                        let local_z = world_z.rem_euclid(16) as usize;
                        let block = chunk
                            .block(local_x, world_y as isize, local_z)
                            .map(|block| block.name())
                            .unwrap_or("minecraft:air");
                        let label = if matches!(
                            block,
                            "minecraft:air" | "minecraft:cave_air" | "minecraft:void_air"
                        ) {
                            0
                        } else if let Some(label) = reverse.get(block) {
                            *label
                        } else {
                            *unknown_counts.entry(block.to_string()).or_insert(0) += 1;
                            if unknown_samples.len() < 12 {
                                unknown_samples.push(json!({
                                    "block": block,
                                    "position": [world_x, world_y, world_z]
                                }));
                            }
                            0
                        };
                        let index = placement
                            .voxel_index([world_x, world_y, world_z], manifest.nifti.dimensions)
                            .expect("world coordinate is within volume bounds");
                        voxels[index] = label;
                    }
                }
            }
        }
    }
    if !unknown_counts.is_empty() {
        let total: u64 = unknown_counts.values().sum();
        return Err(AppError::incompatible(format!(
            "found {total} blocks inside the export volume that are not in the label palette"
        ))
        .with_details(json!({"unknown_blocks": unknown_counts, "samples": unknown_samples})));
    }
    Ok(voxels)
}

fn placement_from_manifest(manifest: &Manifest) -> Result<Placement> {
    let vertical = match manifest.axes.vertical_axis.as_str() {
        "x" => 0,
        "y" => 1,
        "z" => 2,
        value => {
            return Err(AppError::incompatible(format!(
                "invalid vertical axis {value}"
            )));
        }
    };
    let horizontal: Vec<usize> = (0..3).filter(|axis| *axis != vertical).collect();
    let placement = Placement {
        vertical,
        horizontal: [horizontal[0], horizontal[1]],
        bounds: manifest.volume_bounds.clone(),
    };
    let expected = Placement::new(
        manifest.nifti.dimensions,
        [VerticalAxis::X, VerticalAxis::Y, VerticalAxis::Z][vertical],
    )?;
    if placement.bounds != expected.bounds || placement.horizontal != expected.horizontal {
        return Err(AppError::incompatible(
            "manifest axis mapping and volume bounds are inconsistent",
        ));
    }
    Ok(placement)
}

fn ensure_world_unlocked(world: &Path) -> Result<()> {
    let path = world.join("session.lock");
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .map_err(|error| {
            AppError::incompatible(format!("cannot open {}: {error}", path.display()))
        })?;
    file.try_lock_exclusive().map_err(|_| {
        AppError::incompatible(
            "the Minecraft world is currently open; close it before validating or exporting",
        )
    })?;
    FileExt::unlock(&file)
        .map_err(|error| AppError::io(format!("cannot release world lock: {error}")))?;
    Ok(())
}

pub fn palette(world: &Path) -> Result<Vec<PaletteEntry>> {
    Ok(Manifest::load(world)?.palette)
}

pub fn nifti_metadata(world: &Path) -> Result<NiftiMetadata> {
    Ok(Manifest::load(world)?.nifti)
}

pub fn run_cli() {
    crate::cli::main_entry();
}
