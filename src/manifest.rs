use crate::error::{AppError, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

pub const SCHEMA_VERSION: u32 = 2;
pub const MINECRAFT_VERSION: &str = "26.2";
pub const MINECRAFT_DATA_VERSION: i32 = 4903;
pub const MANIFEST_DIR: &str = ".nii2mc";
pub const MANIFEST_FILE: &str = "manifest.json";
pub const PREFIX_FILE: &str = "nifti-prefix.bin";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NiftiMetadata {
    pub dimensions: [u32; 3],
    pub spacing: [f32; 3],
    pub datatype: u16,
    pub bits_per_voxel: u16,
    pub endianness: String,
    pub voxel_offset: u64,
    pub qform_code: i16,
    pub sform_code: i16,
    pub quaternion: [f32; 3],
    pub qoffset: [f32; 3],
    pub srow_x: [f32; 4],
    pub srow_y: [f32; 4],
    pub srow_z: [f32; 4],
    pub spatial_units: u8,
    pub temporal_units: u8,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorldBounds {
    pub min: [i32; 3],
    pub max: [i32; 3],
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct DimensionBounds {
    pub min_y: i32,
    pub height: u32,
}

impl DimensionBounds {
    pub fn max_y(self) -> i32 {
        self.min_y + self.height as i32 - 1
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AxisMapping {
    pub nifti_x_to_minecraft: String,
    pub nifti_y_to_minecraft: String,
    pub nifti_z_to_minecraft: String,
    pub vertical_axis: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChunkBounds {
    pub min_x: i32,
    pub max_x: i32,
    pub min_z: i32,
    pub max_z: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PaletteEntry {
    pub label: u32,
    pub block: String,
    pub name: Option<String>,
    pub voxel_count: u64,
    pub legend_position: [i32; 3],
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub schema_version: u32,
    pub created_by: String,
    pub minecraft_version: String,
    pub minecraft_data_version: i32,
    pub source_filename: String,
    pub source_sha256: String,
    pub prefix_sha256: String,
    pub nifti: NiftiMetadata,
    pub axes: AxisMapping,
    pub dimension_bounds: DimensionBounds,
    pub volume_bounds: WorldBounds,
    pub required_chunks: ChunkBounds,
    pub palette: Vec<PaletteEntry>,
}

impl Manifest {
    pub fn path(world: &Path) -> PathBuf {
        world.join(MANIFEST_DIR).join(MANIFEST_FILE)
    }

    pub fn prefix_path(world: &Path) -> PathBuf {
        world.join(MANIFEST_DIR).join(PREFIX_FILE)
    }

    pub fn load(world: &Path) -> Result<Self> {
        let path = Self::path(world);
        let bytes = fs::read(&path).map_err(|error| {
            AppError::io(format!("cannot read manifest {}: {error}", path.display()))
        })?;
        let manifest: Self = serde_json::from_slice(&bytes).map_err(|error| {
            AppError::incompatible(format!("invalid manifest {}: {error}", path.display()))
        })?;
        if manifest.schema_version != SCHEMA_VERSION {
            return Err(AppError::incompatible(format!(
                "unsupported manifest schema {}; expected {}",
                manifest.schema_version, SCHEMA_VERSION
            )));
        }
        if manifest.minecraft_data_version != MINECRAFT_DATA_VERSION {
            return Err(AppError::incompatible(format!(
                "world targets Minecraft data version {}; expected {} (Java {})",
                manifest.minecraft_data_version, MINECRAFT_DATA_VERSION, MINECRAFT_VERSION
            )));
        }
        Ok(manifest)
    }

    pub fn write(&self, world: &Path) -> Result<()> {
        let directory = world.join(MANIFEST_DIR);
        fs::create_dir_all(&directory)?;
        let bytes = serde_json::to_vec_pretty(self)
            .map_err(|error| AppError::io(format!("cannot serialize manifest: {error}")))?;
        fs::write(directory.join(MANIFEST_FILE), bytes)?;
        Ok(())
    }
}
