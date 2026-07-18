use crate::error::{AppError, Result};
use crate::manifest::MINECRAFT_DATA_VERSION;
use fastanvil::Region;
use fastnbt::{LongArray, Value};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

pub const MIN_Y: i32 = -64;
pub const MAX_Y: i32 = 319;
const MIN_SECTION_Y: i32 = -4;
const MAX_SECTION_Y: i32 = 19;

pub fn region_directory(world: &Path) -> PathBuf {
    world.join("dimensions/minecraft/overworld/region")
}

pub fn region_path(world: &Path, region_x: i32, region_z: i32) -> PathBuf {
    region_directory(world).join(format!("r.{region_x}.{region_z}.mca"))
}

pub fn write_chunks<F>(world: &Path, chunks: &BTreeSet<(i32, i32)>, mut block_at: F) -> Result<()>
where
    F: FnMut(i32, i32, i32) -> Option<String>,
{
    let directory = region_directory(world);
    fs::create_dir_all(&directory)?;
    let mut regions: BTreeMap<(i32, i32), Vec<(i32, i32)>> = BTreeMap::new();
    for (chunk_x, chunk_z) in chunks {
        regions
            .entry((chunk_x.div_euclid(32), chunk_z.div_euclid(32)))
            .or_default()
            .push((*chunk_x, *chunk_z));
    }

    for ((region_x, region_z), region_chunks) in regions {
        let path = region_path(world, region_x, region_z);
        let file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .read(true)
            .write(true)
            .open(&path)
            .map_err(|error| {
                AppError::io(format!("cannot create region {}: {error}", path.display()))
            })?;
        let mut region = Region::create(file).map_err(|error| {
            AppError::io(format!(
                "cannot initialize region {}: {error}",
                path.display()
            ))
        })?;
        for (chunk_x, chunk_z) in region_chunks {
            let chunk = make_chunk(chunk_x, chunk_z, &mut block_at)?;
            let local_x = chunk_x.rem_euclid(32) as usize;
            let local_z = chunk_z.rem_euclid(32) as usize;
            region
                .write_chunk(local_x, local_z, &chunk)
                .map_err(|error| {
                    AppError::io(format!(
                        "cannot write chunk ({chunk_x}, {chunk_z}) to {}: {error}",
                        path.display()
                    ))
                })?;
        }
    }
    Ok(())
}

fn make_chunk<F>(chunk_x: i32, chunk_z: i32, block_at: &mut F) -> Result<Vec<u8>>
where
    F: FnMut(i32, i32, i32) -> Option<String>,
{
    let mut sections = Vec::with_capacity((MAX_SECTION_Y - MIN_SECTION_Y + 1) as usize);
    let mut height = [0u16; 256];
    for section_y in MIN_SECTION_Y..=MAX_SECTION_Y {
        let mut palette = vec!["minecraft:air".to_string()];
        let mut palette_index = HashMap::from([("minecraft:air".to_string(), 0u16)]);
        let mut states = vec![0u16; 4096];
        for local_y in 0..16 {
            let world_y = section_y * 16 + local_y;
            for local_z in 0..16 {
                let world_z = chunk_z * 16 + local_z;
                for local_x in 0..16 {
                    let world_x = chunk_x * 16 + local_x;
                    let Some(block) = block_at(world_x, world_y, world_z) else {
                        continue;
                    };
                    let index = *palette_index.entry(block.clone()).or_insert_with(|| {
                        let index = palette.len() as u16;
                        palette.push(block);
                        index
                    });
                    let state_index = (local_y * 256 + local_z * 16 + local_x) as usize;
                    states[state_index] = index;
                    let column = (local_z * 16 + local_x) as usize;
                    let encoded_height = (world_y + 1 - MIN_Y) as u16;
                    height[column] = height[column].max(encoded_height);
                }
            }
        }
        sections.push(section_value(section_y, palette, &states));
    }

    let packed_height = pack_values(&height, 9);
    let heightmaps = compound([
        (
            "MOTION_BLOCKING",
            Value::LongArray(LongArray::new(packed_height.clone())),
        ),
        (
            "MOTION_BLOCKING_NO_LEAVES",
            Value::LongArray(LongArray::new(packed_height.clone())),
        ),
        (
            "OCEAN_FLOOR",
            Value::LongArray(LongArray::new(packed_height.clone())),
        ),
        (
            "WORLD_SURFACE",
            Value::LongArray(LongArray::new(packed_height)),
        ),
    ]);

    let post_processing = (MIN_SECTION_Y..=MAX_SECTION_Y)
        .map(|_| Value::List(Vec::new()))
        .collect();
    let root = HashMap::from([
        (
            "DataVersion".to_string(),
            Value::Int(MINECRAFT_DATA_VERSION),
        ),
        ("xPos".to_string(), Value::Int(chunk_x)),
        ("yPos".to_string(), Value::Int(MIN_SECTION_Y)),
        ("zPos".to_string(), Value::Int(chunk_z)),
        (
            "Status".to_string(),
            Value::String("minecraft:full".to_string()),
        ),
        ("LastUpdate".to_string(), Value::Long(0)),
        ("InhabitedTime".to_string(), Value::Long(0)),
        ("isLightOn".to_string(), Value::Byte(0)),
        ("sections".to_string(), Value::List(sections)),
        ("Heightmaps".to_string(), heightmaps),
        ("block_entities".to_string(), Value::List(Vec::new())),
        ("block_ticks".to_string(), Value::List(Vec::new())),
        ("fluid_ticks".to_string(), Value::List(Vec::new())),
        ("PostProcessing".to_string(), Value::List(post_processing)),
        (
            "structures".to_string(),
            compound([("starts", compound([])), ("References", compound([]))]),
        ),
    ]);
    fastnbt::to_bytes(&root)
        .map_err(|error| AppError::io(format!("cannot encode chunk NBT: {error}")))
}

fn section_value(section_y: i32, palette: Vec<String>, states: &[u16]) -> Value {
    let palette_values = palette
        .iter()
        .map(|name| compound([("Name", Value::String(name.clone()))]))
        .collect();
    let mut block_states = HashMap::from([("palette".to_string(), Value::List(palette_values))]);
    if palette.len() > 1 {
        let bits = min_bits(palette.len()).max(4);
        block_states.insert(
            "data".to_string(),
            Value::LongArray(LongArray::new(pack_values(states, bits))),
        );
    }
    compound([
        ("Y", Value::Byte(section_y as i8)),
        ("block_states", Value::Compound(block_states)),
        (
            "biomes",
            compound([(
                "palette",
                Value::List(vec![Value::String("minecraft:the_void".to_string())]),
            )]),
        ),
    ])
}

fn min_bits(palette_len: usize) -> usize {
    usize::BITS as usize - (palette_len - 1).leading_zeros() as usize
}

pub fn pack_values(values: &[u16], bits: usize) -> Vec<i64> {
    let per_long = 64 / bits;
    let mut packed = vec![0u64; values.len().div_ceil(per_long)];
    let mask = if bits == 64 {
        u64::MAX
    } else {
        (1u64 << bits) - 1
    };
    for (index, value) in values.iter().enumerate() {
        let long_index = index / per_long;
        let position = (index % per_long) * bits;
        packed[long_index] |= ((*value as u64) & mask) << position;
    }
    packed.into_iter().map(|value| value as i64).collect()
}

pub fn read_chunk(world: &Path, chunk_x: i32, chunk_z: i32) -> Result<Option<Vec<u8>>> {
    let region_x = chunk_x.div_euclid(32);
    let region_z = chunk_z.div_euclid(32);
    let path = region_path(world, region_x, region_z);
    if !path.exists() {
        return Ok(None);
    }
    let file = File::open(&path)?;
    let mut region = Region::from_stream(file).map_err(|error| {
        AppError::incompatible(format!("invalid region {}: {error}", path.display()))
    })?;
    region
        .read_chunk(
            chunk_x.rem_euclid(32) as usize,
            chunk_z.rem_euclid(32) as usize,
        )
        .map_err(|error| {
            AppError::incompatible(format!(
                "cannot read chunk ({chunk_x}, {chunk_z}) from {}: {error}",
                path.display()
            ))
        })
}

pub fn write_gzip_nbt(path: &Path, value: &HashMap<String, Value>) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let bytes = fastnbt::to_bytes(value)
        .map_err(|error| AppError::io(format!("cannot encode {}: {error}", path.display())))?;
    let file = File::create(path)?;
    let mut encoder = flate2::write::GzEncoder::new(file, flate2::Compression::default());
    encoder.write_all(&bytes)?;
    encoder.finish()?;
    Ok(())
}

pub fn compound<const N: usize>(entries: [(&str, Value); N]) -> Value {
    Value::Compound(
        entries
            .into_iter()
            .map(|(key, value)| (key.to_string(), value))
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use fastanvil::{Chunk, CurrentJavaChunk};

    #[test]
    fn packed_values_round_trip_through_fastanvil() {
        let chunk = make_chunk(0, 0, &mut |x, y, z| {
            (x == 3 && y == -10 && z == 4).then(|| "minecraft:red_concrete".to_string())
        })
        .unwrap();
        let chunk: CurrentJavaChunk = fastnbt::from_bytes(&chunk).unwrap();
        assert_eq!(
            chunk.block(3, -10, 4).unwrap().name(),
            "minecraft:red_concrete"
        );
        assert_eq!(chunk.block(2, -10, 4).unwrap().name(), "minecraft:air");
    }
}
