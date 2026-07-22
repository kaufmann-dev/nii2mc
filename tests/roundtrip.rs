use assert_cmd::Command;
use fastanvil::{Chunk, CurrentJavaChunk, Region};
use fastnbt::Value;
use flate2::read::GzDecoder;
use nii2mc::manifest::Manifest;
use nii2mc::nifti::read_nifti;
use nii2mc::world::{VerticalAxis, create_world, export_nifti, validate_world};
use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::Read;
use std::path::Path;

#[test]
fn multilabel_nifti_round_trips_with_prefix_and_names() {
    let temporary = tempfile::tempdir().unwrap();
    let source = temporary.path().join("labels.nii");
    let labels = vec![0, 1, 2, 2, 1, 0, 2, 0, 1, 0, 0, 2];
    write_test_nifti(&source, [3, 2, 2], &labels);

    let original = read_nifti(&source).unwrap();
    assert_eq!(original.label_names.get(&1).unwrap(), "femur_left");
    assert_eq!(original.label_names.get(&2).unwrap(), "aorta");

    let world = temporary.path().join("labels-world");
    create_world(&source, &world, VerticalAxis::Z).unwrap();
    let manifest = Manifest::load(&world).unwrap();
    assert_eq!(manifest.palette[0].block, "minecraft:bone_block");
    assert_eq!(manifest.palette[1].block, "minecraft:red_concrete");
    let report = validate_world(&world).unwrap();
    assert!(report.valid);
    assert_eq!(report.checked_voxels, labels.len() as u64);

    let output = temporary.path().join("roundtrip.nii.gz");
    export_nifti(&world, &output).unwrap();
    let roundtrip = read_nifti(&output).unwrap();
    assert_eq!(
        roundtrip.voxels,
        labels.into_iter().map(u32::from).collect::<Vec<_>>()
    );
    assert_eq!(roundtrip.prefix, original.prefix);
    assert_eq!(roundtrip.metadata, original.metadata);
    assert_eq!(roundtrip.label_names, original.label_names);
}

#[test]
fn doctor_json_has_a_stable_success_envelope() {
    let output = Command::cargo_bin("nii2mc")
        .unwrap()
        .args(["--json", "doctor"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["ok"], true);
    assert_eq!(value["command"], "doctor");
    assert_eq!(value["data"]["minecraft"]["version"], "26.2");
    assert_eq!(value["data"]["nifti"]["maximum_vertical_voxels"], 4064);
    assert!(output.stderr.is_empty());
}

#[test]
fn unknown_blocks_abort_validation_and_export() {
    let temporary = tempfile::tempdir().unwrap();
    let source = temporary.path().join("labels.nii");
    let labels = vec![0, 1, 2, 2, 1, 0, 2, 0, 1, 0, 0, 2];
    write_test_nifti(&source, [3, 2, 2], &labels);
    let world = temporary.path().join("labels-world");
    create_world(&source, &world, VerticalAxis::Z).unwrap();

    rename_block_in_region(&world, 0, -1, "minecraft:bone_block", "minecraft:dirt");
    let validation = validate_world(&world).unwrap_err();
    assert!(validation.message.contains("not in the label palette"));
    assert!(validation.details.is_some());

    let output = temporary.path().join("must-not-exist.nii.gz");
    assert!(export_nifti(&world, &output).is_err());
    assert!(!output.exists());
}

#[test]
fn tall_volume_uses_a_custom_height_and_round_trips() {
    let temporary = tempfile::tempdir().unwrap();
    let source = temporary.path().join("tall-labels.nii");
    let labels = vec![1; 622];
    write_test_nifti(&source, [1, 1, 622], &labels);

    let world = temporary.path().join("tall-world");
    create_world(&source, &world, VerticalAxis::Z).unwrap();

    let manifest = Manifest::load(&world).unwrap();
    assert_eq!(manifest.dimension_bounds.min_y, -304);
    assert_eq!(manifest.dimension_bounds.height, 624);
    assert_eq!(manifest.volume_bounds.min, [0, -303, 0]);
    assert_eq!(manifest.volume_bounds.max, [0, 318, 0]);

    let dimension_type: serde_json::Value = serde_json::from_slice(
        &fs::read(world.join("datapacks/nii2mc/data/nii2mc/dimension_type/overworld.json"))
            .unwrap(),
    )
    .unwrap();
    assert_eq!(dimension_type["min_y"], -304);
    assert_eq!(dimension_type["height"], 624);
    assert_eq!(dimension_type["logical_height"], 624);

    let level = read_gzip_nbt(&world.join("level.dat"));
    let Value::Compound(data) = &level["Data"] else {
        panic!("level.dat Data is not a compound");
    };
    let Value::Compound(data_packs) = &data["DataPacks"] else {
        panic!("level.dat DataPacks is not a compound");
    };
    let Value::List(enabled) = &data_packs["Enabled"] else {
        panic!("level.dat enabled data packs is not a list");
    };
    assert!(enabled.contains(&Value::String("file/nii2mc".to_string())));

    let world_gen = read_gzip_nbt(&world.join("data/minecraft/world_gen_settings.dat"));
    let Value::Compound(world_gen_data) = &world_gen["data"] else {
        panic!("world generation data is not a compound");
    };
    let Value::Compound(dimensions) = &world_gen_data["dimensions"] else {
        panic!("dimensions is not a compound");
    };
    let Value::Compound(overworld) = &dimensions["minecraft:overworld"] else {
        panic!("Overworld settings are not a compound");
    };
    assert_eq!(
        overworld["type"],
        Value::String("nii2mc:overworld".to_string())
    );

    let game_rules = read_gzip_nbt(&world.join("data/minecraft/game_rules.dat"));
    let Value::Compound(game_rule_data) = &game_rules["data"] else {
        panic!("game rule data is not a compound");
    };
    assert_eq!(game_rule_data["minecraft:advance_time"], Value::Byte(0));
    assert_eq!(game_rule_data["minecraft:random_tick_speed"], Value::Int(0));
    assert_eq!(game_rule_data["minecraft:keep_inventory"], Value::Byte(1));
    assert!(!game_rule_data.contains_key("rules"));

    let chunk = read_chunk(&world, 0, 0);
    assert_eq!(chunk.y_range(), -304..320);
    assert_eq!(
        chunk.block(0, -303, 0).unwrap().name(),
        "minecraft:bone_block"
    );
    assert_eq!(
        chunk.block(0, 318, 0).unwrap().name(),
        "minecraft:bone_block"
    );

    let report = validate_world(&world).unwrap();
    assert_eq!(report.checked_voxels, 622);
    let output = temporary.path().join("tall-roundtrip.nii");
    export_nifti(&world, &output).unwrap();
    assert_eq!(read_nifti(&output).unwrap().voxels, vec![1; 622]);
}

#[test]
fn volume_taller_than_the_custom_dimension_limit_is_rejected() {
    let temporary = tempfile::tempdir().unwrap();
    let source = temporary.path().join("too-tall.nii");
    write_test_nifti(&source, [1, 1, 4065], &vec![1; 4065]);

    let error = create_world(
        &source,
        &temporary.path().join("must-not-exist"),
        VerticalAxis::Z,
    )
    .unwrap_err();
    assert!(error.message.contains("4,064-block"));
    assert!(error.message.contains("fitting axes: x, y"));
}

fn write_test_nifti(path: &Path, dimensions: [u16; 3], labels: &[u8]) {
    let xml = br#"<?xml version="1.0"?><CaretExtension><LabelTable><Label Key="0">background</Label><Label Key="1">femur_left</Label><Label Key="2">aorta</Label></LabelTable></CaretExtension>"#;
    let extension_size = (xml.len() + 8).div_ceil(16) * 16;
    let voxel_offset = 352 + extension_size;
    let mut prefix = vec![0u8; voxel_offset];
    put_i32(&mut prefix, 0, 348);
    put_i16(&mut prefix, 40, 3);
    put_i16(&mut prefix, 42, dimensions[0] as i16);
    put_i16(&mut prefix, 44, dimensions[1] as i16);
    put_i16(&mut prefix, 46, dimensions[2] as i16);
    put_i16(&mut prefix, 70, 2);
    put_i16(&mut prefix, 72, 8);
    put_f32(&mut prefix, 76, 1.0);
    put_f32(&mut prefix, 80, 0.5);
    put_f32(&mut prefix, 84, 0.6);
    put_f32(&mut prefix, 88, 1.2);
    put_f32(&mut prefix, 108, voxel_offset as f32);
    prefix[123] = 2;
    prefix[148..162].copy_from_slice(b"nii2mc fixture");
    put_i16(&mut prefix, 254, 1);
    put_f32(&mut prefix, 280, 0.5);
    put_f32(&mut prefix, 300, 0.6);
    put_f32(&mut prefix, 320, 1.2);
    prefix[344..348].copy_from_slice(b"n+1\0");
    prefix[348] = 1;
    put_i32(&mut prefix, 352, extension_size as i32);
    put_i32(&mut prefix, 356, 6);
    prefix[360..360 + xml.len()].copy_from_slice(xml);
    let mut file = prefix;
    file.extend_from_slice(labels);
    fs::write(path, file).unwrap();
}

fn put_i16(bytes: &mut [u8], offset: usize, value: i16) {
    bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

fn put_i32(bytes: &mut [u8], offset: usize, value: i32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn put_f32(bytes: &mut [u8], offset: usize, value: f32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn rename_block_in_region(
    world: &Path,
    chunk_x: i32,
    chunk_z: i32,
    old_name: &str,
    new_name: &str,
) {
    let region_path = world.join(format!(
        "dimensions/minecraft/overworld/region/r.{}.{}.mca",
        chunk_x.div_euclid(32),
        chunk_z.div_euclid(32)
    ));
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(region_path)
        .unwrap();
    let mut region = Region::from_stream(file).unwrap();
    let bytes = region
        .read_chunk(
            chunk_x.rem_euclid(32) as usize,
            chunk_z.rem_euclid(32) as usize,
        )
        .unwrap()
        .unwrap();
    let mut root: HashMap<String, Value> = fastnbt::from_bytes(&bytes).unwrap();
    let Value::List(sections) = root.get_mut("sections").unwrap() else {
        panic!("sections is not a list");
    };
    let mut changed = false;
    for section in sections {
        let Value::Compound(section) = section else {
            continue;
        };
        let Some(Value::Compound(block_states)) = section.get_mut("block_states") else {
            continue;
        };
        let Some(Value::List(palette)) = block_states.get_mut("palette") else {
            continue;
        };
        for entry in palette {
            let Value::Compound(entry) = entry else {
                continue;
            };
            if entry.get("Name") == Some(&Value::String(old_name.to_string())) {
                entry.insert("Name".to_string(), Value::String(new_name.to_string()));
                changed = true;
            }
        }
    }
    assert!(changed, "source block was not found in the chunk palette");
    let updated = fastnbt::to_bytes(&root).unwrap();
    region
        .write_chunk(
            chunk_x.rem_euclid(32) as usize,
            chunk_z.rem_euclid(32) as usize,
            &updated,
        )
        .unwrap();
}

fn read_chunk(world: &Path, chunk_x: i32, chunk_z: i32) -> CurrentJavaChunk {
    let region_path = world.join(format!(
        "dimensions/minecraft/overworld/region/r.{}.{}.mca",
        chunk_x.div_euclid(32),
        chunk_z.div_euclid(32)
    ));
    let file = OpenOptions::new().read(true).open(region_path).unwrap();
    let mut region = Region::from_stream(file).unwrap();
    let bytes = region
        .read_chunk(
            chunk_x.rem_euclid(32) as usize,
            chunk_z.rem_euclid(32) as usize,
        )
        .unwrap()
        .unwrap();
    fastnbt::from_bytes(&bytes).unwrap()
}

fn read_gzip_nbt(path: &Path) -> HashMap<String, Value> {
    let mut decoder = GzDecoder::new(fs::File::open(path).unwrap());
    let mut bytes = Vec::new();
    decoder.read_to_end(&mut bytes).unwrap();
    fastnbt::from_bytes(&bytes).unwrap()
}
