//! Opt-in resampling, anatomical orientation, naming, and palette overrides.

use assert_cmd::Command;
use fastanvil::{Chunk, CurrentJavaChunk, Region};
use fastnbt::Value;
use flate2::read::GzDecoder;
use nii2mc::manifest::Manifest;
use nii2mc::nifti::read_nifti;
use nii2mc::palette::parse_palette_overrides;
use nii2mc::resample::{BlockSize, Orientation, TransformOptions};
use nii2mc::world::{
    VerticalAxis, WorldOptions, create_world, create_world_with, export_nifti, resample_nifti,
    validate_world,
};
use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::Read;
use std::path::Path;

const TOTALSEGMENTATOR_XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<CaretExtension><Date><![CDATA[2026-01-01T00:00:00]]></Date><VolumeInformation Index="0"><LabelTable><Label Key="1" Red="1" Green="0" Blue="0" Alpha="1"><![CDATA[spleen]]></Label><Label Key="2" Red="0" Green="1" Blue="0" Alpha="1"><![CDATA[kidney_right]]></Label><Label Key="3" Red="0" Green="0" Blue="1" Alpha="1">vertebrae_L1 &amp; disc</Label></LabelTable></VolumeInformation></CaretExtension>"#;

struct Fixture<'a> {
    dimensions: [u16; 3],
    spacing: [f32; 3],
    /// sform rows; `None` writes no orientation at all.
    sform: Option<[[f32; 4]; 3]>,
    xml: &'a str,
    ecode: i32,
}

fn write_fixture(path: &Path, fixture: &Fixture, labels: &[u8]) {
    let xml = fixture.xml.as_bytes();
    let extension_size = if xml.is_empty() {
        0
    } else {
        (xml.len() + 8).div_ceil(16) * 16
    };
    let voxel_offset = 352 + extension_size;
    let mut prefix = vec![0u8; voxel_offset];
    put_i32(&mut prefix, 0, 348);
    put_i16(&mut prefix, 40, 3);
    for axis in 0..3 {
        put_i16(&mut prefix, 42 + 2 * axis, fixture.dimensions[axis] as i16);
        put_f32(&mut prefix, 80 + 4 * axis, fixture.spacing[axis]);
    }
    put_i16(&mut prefix, 70, 2);
    put_i16(&mut prefix, 72, 8);
    put_f32(&mut prefix, 76, 1.0);
    put_f32(&mut prefix, 108, voxel_offset as f32);
    prefix[123] = 2;
    if let Some(rows) = fixture.sform {
        put_i16(&mut prefix, 254, 1);
        for (row, values) in rows.iter().enumerate() {
            for (column, value) in values.iter().enumerate() {
                put_f32(&mut prefix, 280 + 16 * row + 4 * column, *value);
            }
        }
    }
    prefix[344..348].copy_from_slice(b"n+1\0");
    if !xml.is_empty() {
        prefix[348] = 1;
        put_i32(&mut prefix, 352, extension_size as i32);
        put_i32(&mut prefix, 356, fixture.ecode);
        prefix[360..360 + xml.len()].copy_from_slice(xml);
    }
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

fn diagonal(spacing: [f32; 3]) -> [[f32; 4]; 3] {
    [
        [spacing[0], 0.0, 0.0, 0.0],
        [0.0, spacing[1], 0.0, 0.0],
        [0.0, 0.0, spacing[2], 0.0],
    ]
}

/// Sphere of radius `radius_mm` centred in a grid with the given spacing.
fn sphere(dimensions: [u16; 3], spacing: [f32; 3], radius_mm: f32) -> Vec<u8> {
    let center: Vec<f32> = (0..3)
        .map(|axis| (dimensions[axis] as f32 - 1.0) * spacing[axis] / 2.0)
        .collect();
    let mut labels = Vec::new();
    for k in 0..dimensions[2] {
        for j in 0..dimensions[1] {
            for i in 0..dimensions[0] {
                let point = [
                    i as f32 * spacing[0] - center[0],
                    j as f32 * spacing[1] - center[1],
                    k as f32 * spacing[2] - center[2],
                ];
                let inside =
                    point.iter().map(|value| value * value).sum::<f32>() <= radius_mm * radius_mm;
                labels.push(u8::from(inside));
            }
        }
    }
    labels
}

fn extent(voxels: &[u32], dims: [u32; 3], label: u32) -> [u32; 3] {
    let mut low = [u32::MAX; 3];
    let mut high = [0u32; 3];
    for k in 0..dims[2] {
        for j in 0..dims[1] {
            for i in 0..dims[0] {
                let index = (i + dims[0] * (j + dims[1] * k)) as usize;
                if voxels[index] == label {
                    for (axis, value) in [i, j, k].into_iter().enumerate() {
                        low[axis] = low[axis].min(value);
                        high[axis] = high[axis].max(value);
                    }
                }
            }
        }
    }
    [0, 1, 2].map(|axis| high[axis] - low[axis] + 1)
}

fn centroid(voxels: &[u32], dims: [u32; 3], label: u32) -> [f64; 3] {
    let mut sum = [0.0; 3];
    let mut count = 0.0;
    for k in 0..dims[2] {
        for j in 0..dims[1] {
            for i in 0..dims[0] {
                if voxels[(i + dims[0] * (j + dims[1] * k)) as usize] == label {
                    sum[0] += f64::from(i);
                    sum[1] += f64::from(j);
                    sum[2] += f64::from(k);
                    count += 1.0;
                }
            }
        }
    }
    sum.map(|value| value / count)
}

fn options(block: Option<f64>, orientation: Orientation) -> TransformOptions {
    TransformOptions {
        block: block.map(BlockSize::Millimetres),
        orientation,
        ..TransformOptions::default()
    }
}

#[test]
fn totalsegmentator_cdata_label_names_are_read() {
    let temporary = tempfile::tempdir().unwrap();
    let source = temporary.path().join("segmentations.nii");
    let fixture = Fixture {
        dimensions: [4, 4, 4],
        spacing: [1.0; 3],
        sform: Some(diagonal([1.0; 3])),
        xml: TOTALSEGMENTATOR_XML,
        ecode: 0,
    };
    let mut labels = vec![0u8; 64];
    labels[0] = 1;
    labels[5] = 2;
    labels[10] = 3;
    write_fixture(&source, &fixture, &labels);

    let volume = read_nifti(&source).unwrap();
    assert_eq!(volume.label_names.get(&1).unwrap(), "spleen");
    assert_eq!(volume.label_names.get(&2).unwrap(), "kidney_right");
    assert_eq!(volume.label_names.get(&3).unwrap(), "vertebrae_L1 & disc");

    let world = temporary.path().join("world");
    create_world(&source, &world, VerticalAxis::Z).unwrap();
    let manifest = Manifest::load(&world).unwrap();
    assert_eq!(manifest.palette[0].name.as_deref(), Some("spleen"));
    // Named vertebrae get pale bone blocks instead of the generic order.
    assert_eq!(manifest.palette[2].block, "minecraft:bone_block");
}

#[test]
fn block_resampling_makes_anisotropic_spheres_round_and_keeps_volume() {
    let temporary = tempfile::tempdir().unwrap();
    let source = temporary.path().join("sphere.nii");
    let spacing = [0.5, 0.5, 2.5];
    let dimensions = [60, 60, 12];
    let labels = sphere(dimensions, spacing, 12.0);
    let source_volume = labels.iter().filter(|value| **value == 1).count() as f64 * 0.625;
    write_fixture(
        &source,
        &Fixture {
            dimensions,
            spacing,
            sform: Some(diagonal(spacing)),
            xml: "",
            ecode: 0,
        },
        &labels,
    );

    // One voxel per block: the sphere is 48 blocks wide but only ~10 tall.
    let original = read_nifti(&source).unwrap();
    let raw = extent(&original.voxels, original.metadata.dimensions, 1);
    assert!(raw[0] > 4 * raw[2]);

    let resampled = temporary.path().join("sphere-1mm.nii.gz");
    let report =
        resample_nifti(&source, &resampled, &options(Some(1.0), Orientation::Voxel)).unwrap();
    let derived = read_nifti(&resampled).unwrap();
    assert_eq!(derived.metadata.spacing, [1.0, 1.0, 1.0]);
    assert_eq!(report.dimensions, [30, 30, 30]);
    let blocks = extent(&derived.voxels, derived.metadata.dimensions, 1);
    for axis in 0..3 {
        assert!((blocks[axis] as i64 - 24).abs() <= 2, "extent {blocks:?}");
    }
    let block_volume = derived.voxels.iter().filter(|value| **value == 1).count() as f64;
    assert!(
        (block_volume / source_volume - 1.0).abs() < 0.03,
        "{block_volume} vs {source_volume}"
    );
}

#[test]
fn thin_structures_survive_downsampling_by_coverage() {
    let temporary = tempfile::tempdir().unwrap();
    let source = temporary.path().join("slab.nii");
    // A 1 mm slab (two 0.5 mm voxels) resampled to 1 mm blocks stays one block thick.
    let dimensions = [20u16, 20, 20];
    let mut labels = vec![0u8; 8000];
    for k in 0..20usize {
        for j in 0..20usize {
            for i in 8..10usize {
                labels[i + 20 * (j + 20 * k)] = 7;
            }
        }
    }
    write_fixture(
        &source,
        &Fixture {
            dimensions,
            spacing: [0.5; 3],
            sform: Some(diagonal([0.5; 3])),
            xml: "",
            ecode: 0,
        },
        &labels,
    );
    let output = temporary.path().join("slab-1mm.nii");
    resample_nifti(&source, &output, &options(Some(1.0), Orientation::Voxel)).unwrap();
    let derived = read_nifti(&output).unwrap();
    let blocks = extent(&derived.voxels, derived.metadata.dimensions, 7);
    assert_eq!(blocks, [1, 10, 10]);
}

/// Physical space is RAS+. Voxel axes run i -> Left (x decreasing),
/// j -> Anterior, k -> Superior, like many DICOM-derived NIfTI files.
fn oriented_fixture(path: &Path) {
    let dimensions = [12u16, 10, 14];
    let mut labels = vec![0u8; 12 * 10 * 14];
    let set = |labels: &mut Vec<u8>, i: usize, j: usize, k: usize, value: u8| {
        labels[i + 12 * (j + 10 * k)] = value;
    };
    for k in 4..10 {
        for j in 3..7 {
            for i in 4..8 {
                set(&mut labels, i, j, k, 1);
            }
        }
    }
    // Label 2 marks the patient's right: small i (x decreases with i).
    for k in 5..8 {
        for j in 4..6 {
            set(&mut labels, 1, j, k, 2);
        }
    }
    // Label 3 marks superior (large k); label 4 marks posterior (small j).
    for i in 5..7 {
        set(&mut labels, i, 5, 13, 3);
        set(&mut labels, i, 0, 6, 4);
    }
    write_fixture(
        path,
        &Fixture {
            dimensions,
            spacing: [1.0, 1.0, 2.0],
            sform: Some([
                [-1.0, 0.0, 0.0, 30.0],
                [0.0, 1.0, 0.0, -10.0],
                [0.0, 0.0, 2.0, 5.0],
            ]),
            xml: "",
            ecode: 0,
        },
        &labels,
    );
}

fn assert_anatomical_axes(voxels: &[u32], dims: [u32; 3]) {
    let body = centroid(voxels, dims, 1);
    let right = centroid(voxels, dims, 2);
    let superior = centroid(voxels, dims, 3);
    let posterior = centroid(voxels, dims, 4);
    // Derived index axes: +i Right, +j Posterior, +k Superior.
    assert!(right[0] > body[0] + 2.0, "right {right:?} body {body:?}");
    assert!(
        superior[2] > body[2] + 2.0,
        "superior {superior:?} body {body:?}"
    );
    assert!(
        posterior[1] > body[1] + 1.0,
        "posterior {posterior:?} body {body:?}"
    );
}

#[test]
fn anatomical_orientation_permutes_without_resampling() {
    let temporary = tempfile::tempdir().unwrap();
    let source = temporary.path().join("oriented.nii");
    oriented_fixture(&source);
    let output = temporary.path().join("anatomical.nii");
    let report = resample_nifti(&source, &output, &options(None, Orientation::Anatomical)).unwrap();
    let derived = read_nifti(&output).unwrap();
    assert_eq!(report.dimensions, [12, 10, 14]);
    assert_eq!(derived.voxels.len(), 12 * 10 * 14);
    assert_eq!(derived.metadata.spacing, [1.0, 1.0, 2.0]);
    assert_anatomical_axes(&derived.voxels, derived.metadata.dimensions);
    // Voxel count per label is unchanged by a pure permutation.
    let original = read_nifti(&source).unwrap();
    assert_eq!(original.counts, derived.counts);
}

#[test]
fn anatomical_resampling_builds_an_upright_unmirrored_world() {
    let temporary = tempfile::tempdir().unwrap();
    let source = temporary.path().join("oriented.nii");
    oriented_fixture(&source);
    let derived_path = temporary.path().join("derived.nii");
    resample_nifti(
        &source,
        &derived_path,
        &options(Some(1.0), Orientation::Anatomical),
    )
    .unwrap();
    let derived = read_nifti(&derived_path).unwrap();
    assert_anatomical_axes(&derived.voxels, derived.metadata.dimensions);

    let world = temporary.path().join("world");
    let report = create_world_with(
        &source,
        &world,
        &WorldOptions {
            transform: options(Some(1.0), Orientation::Anatomical),
            ..WorldOptions::default()
        },
    )
    .unwrap();
    assert!(report.transform.is_some());
    let manifest = Manifest::load(&world).unwrap();
    let block = |label: u32| {
        manifest
            .palette
            .iter()
            .find(|entry| entry.label == label)
            .unwrap()
            .block
            .clone()
    };
    let body = block_centroid(&world, &manifest, &block(1));
    let right = block_centroid(&world, &manifest, &block(2));
    let superior = block_centroid(&world, &manifest, &block(3));
    let posterior = block_centroid(&world, &manifest, &block(4));
    // Minecraft: +X east = patient right, +Y up = superior, +Z south = posterior.
    assert!(right[0] > body[0] + 2.0);
    assert!(superior[1] > body[1] + 2.0);
    assert!(posterior[2] > body[2] + 1.0);
    assert!(validate_world(&world).unwrap().valid);
}

#[test]
fn scaled_world_round_trips_on_the_block_grid_with_names() {
    let temporary = tempfile::tempdir().unwrap();
    let source = temporary.path().join("labels.nii");
    let spacing = [0.5, 0.5, 2.5];
    let dimensions = [40u16, 40, 8];
    let mut labels = sphere(dimensions, spacing, 8.0);
    for value in labels.iter_mut().take(4000) {
        if *value == 1 {
            *value = 3;
        }
    }
    write_fixture(
        &source,
        &Fixture {
            dimensions,
            spacing,
            sform: Some(diagonal(spacing)),
            xml: TOTALSEGMENTATOR_XML,
            ecode: 0,
        },
        &labels,
    );
    let derived_path = temporary.path().join("derived.nii");
    let transform = TransformOptions {
        block: Some(BlockSize::Millimetres(1.0)),
        crop: true,
        ..TransformOptions::default()
    };
    resample_nifti(&source, &derived_path, &transform).unwrap();
    let derived = read_nifti(&derived_path).unwrap();

    let world = temporary.path().join("world");
    create_world_with(
        &source,
        &world,
        &WorldOptions {
            transform,
            world_name: Some("My scan".to_string()),
            source_name: Some("scan-labels.nii".to_string()),
            ..WorldOptions::default()
        },
    )
    .unwrap();
    let manifest = Manifest::load(&world).unwrap();
    assert_eq!(manifest.source_filename, "scan-labels.nii");
    assert_eq!(manifest.nifti.dimensions, derived.metadata.dimensions);
    assert!(manifest.transform.as_ref().unwrap().cropped);

    let level = read_gzip_nbt(&world.join("level.dat"));
    let Value::Compound(data) = &level["Data"] else {
        panic!("level.dat Data is not a compound");
    };
    assert_eq!(data["LevelName"], Value::String("My scan".to_string()));
    let Value::Compound(spawn) = &data["spawn"] else {
        panic!("level.dat has no spawn compound");
    };
    assert_eq!(
        spawn["dimension"],
        Value::String("minecraft:overworld".to_string())
    );

    let exported = temporary.path().join("exported.nii.gz");
    export_nifti(&world, &exported).unwrap();
    let roundtrip = read_nifti(&exported).unwrap();
    assert_eq!(roundtrip.voxels, derived.voxels);
    assert_eq!(roundtrip.metadata, derived.metadata);
    assert_eq!(
        roundtrip.label_names.get(&3).unwrap(),
        "vertebrae_L1 & disc"
    );
}

#[test]
fn default_options_reproduce_the_original_world() {
    let temporary = tempfile::tempdir().unwrap();
    let source = temporary.path().join("labels.nii");
    let spacing = [0.5, 0.6, 1.2];
    write_fixture(
        &source,
        &Fixture {
            dimensions: [20, 20, 20],
            spacing,
            sform: Some(diagonal(spacing)),
            xml: TOTALSEGMENTATOR_XML,
            ecode: 0,
        },
        &sphere([20, 20, 20], spacing, 4.0),
    );
    let first = temporary.path().join("first");
    let second = temporary.path().join("second");
    create_world(&source, &first, VerticalAxis::Z).unwrap();
    create_world_with(&source, &second, &WorldOptions::default()).unwrap();
    // Region headers carry write timestamps, so compare every stored chunk.
    let region = "dimensions/minecraft/overworld/region";
    for entry in fs::read_dir(first.join(region)).unwrap() {
        let name = entry.unwrap().file_name();
        let open = |root: &Path| {
            Region::from_stream(
                OpenOptions::new()
                    .read(true)
                    .open(root.join(region).join(&name))
                    .unwrap(),
            )
            .unwrap()
        };
        let (mut left, mut right) = (open(&first), open(&second));
        for x in 0..32 {
            for z in 0..32 {
                // NBT compounds serialize in hash order, so compare parsed values.
                let parse = |bytes: Option<Vec<u8>>| {
                    bytes.map(|bytes| fastnbt::from_bytes::<Value>(&bytes).unwrap())
                };
                assert_eq!(
                    parse(left.read_chunk(x, z).unwrap()),
                    parse(right.read_chunk(x, z).unwrap())
                );
            }
        }
    }
    let manifest = fs::read_to_string(first.join(".nii2mc/manifest.json")).unwrap();
    assert!(!manifest.contains("transform"));
    assert_eq!(
        manifest.replace("first", "second"),
        fs::read_to_string(second.join(".nii2mc/manifest.json")).unwrap()
    );
}

#[test]
fn palette_overrides_and_invalid_combinations() {
    let temporary = tempfile::tempdir().unwrap();
    let source = temporary.path().join("segmentations.nii");
    let mut labels = vec![0u8; 64];
    labels[0] = 1;
    labels[5] = 2;
    write_fixture(
        &source,
        &Fixture {
            dimensions: [4, 4, 4],
            spacing: [1.0; 3],
            sform: Some(diagonal([1.0; 3])),
            xml: TOTALSEGMENTATOR_XML,
            ecode: 0,
        },
        &labels,
    );
    let world = temporary.path().join("world");
    create_world_with(
        &source,
        &world,
        &WorldOptions {
            palette: parse_palette_overrides(
                br#"{"1": {"block": "cherry_planks", "name": "Milz"}}"#,
            )
            .unwrap(),
            ..WorldOptions::default()
        },
    )
    .unwrap();
    let manifest = Manifest::load(&world).unwrap();
    assert_eq!(manifest.palette[0].block, "minecraft:cherry_planks");
    assert_eq!(manifest.palette[0].name.as_deref(), Some("Milz"));

    let error = create_world_with(
        &source,
        &temporary.path().join("bad"),
        &WorldOptions {
            vertical_axis: Some(VerticalAxis::X),
            transform: options(None, Orientation::Anatomical),
            ..WorldOptions::default()
        },
    )
    .unwrap_err();
    assert!(error.message.contains("omit --vertical-axis"));

    let unoriented = temporary.path().join("unoriented.nii");
    write_fixture(
        &unoriented,
        &Fixture {
            dimensions: [4, 4, 4],
            spacing: [1.0; 3],
            sform: None,
            xml: "",
            ecode: 0,
        },
        &labels,
    );
    let error = resample_nifti(
        &unoriented,
        &temporary.path().join("x.nii"),
        &options(None, Orientation::Anatomical),
    )
    .unwrap_err();
    assert!(error.message.contains("sform or qform"));
}

#[test]
fn cli_exposes_blocks_resample_and_world_flags() {
    let output = Command::cargo_bin("nii2mc")
        .unwrap()
        .args(["--json", "blocks"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let blocks = value["data"].as_array().unwrap();
    assert_eq!(blocks.len(), 127);
    assert!(
        blocks
            .iter()
            .any(|entry| entry["block"] == "minecraft:bone_block"
                && entry["hex"].as_str().unwrap().starts_with('#'))
    );

    let temporary = tempfile::tempdir().unwrap();
    let source = temporary.path().join("oriented.nii");
    oriented_fixture(&source);
    let palette = temporary.path().join("palette.json");
    fs::write(&palette, r#"{"1": "bone_block"}"#).unwrap();
    let world = temporary.path().join("world");
    let output = Command::cargo_bin("nii2mc")
        .unwrap()
        .args(["--json", "to-world"])
        .arg(&source)
        .arg("--output")
        .arg(&world)
        .args(["--block-mm", "auto", "--orient", "anatomical", "--crop"])
        .args(["--world-name", "Model"])
        .arg("--palette")
        .arg(&palette)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["data"]["transform"]["orientation"], "anatomical");
    assert_eq!(value["data"]["transform"]["block_mm"], 1.0);

    let output = Command::cargo_bin("nii2mc")
        .unwrap()
        .args(["--json", "resample"])
        .arg(&source)
        .arg("--output")
        .arg(temporary.path().join("r.nii"))
        .args(["--fill-threshold", "0.4"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
}

fn block_centroid(world: &Path, manifest: &Manifest, block: &str) -> [f64; 3] {
    let mut sum = [0.0; 3];
    let mut count = 0.0;
    let bounds = &manifest.volume_bounds;
    for chunk_z in manifest.required_chunks.min_z..=manifest.required_chunks.max_z {
        for chunk_x in manifest.required_chunks.min_x..=manifest.required_chunks.max_x {
            let chunk = read_chunk(world, chunk_x, chunk_z);
            for z in bounds.min[2].max(chunk_z * 16)..=bounds.max[2].min(chunk_z * 16 + 15) {
                for x in bounds.min[0].max(chunk_x * 16)..=bounds.max[0].min(chunk_x * 16 + 15) {
                    for y in bounds.min[1]..=bounds.max[1] {
                        let name = chunk
                            .block(
                                x.rem_euclid(16) as usize,
                                y as isize,
                                z.rem_euclid(16) as usize,
                            )
                            .map(|value| value.name().to_string());
                        if name.as_deref() == Some(block) {
                            sum[0] += f64::from(x);
                            sum[1] += f64::from(y);
                            sum[2] += f64::from(z);
                            count += 1.0;
                        }
                    }
                }
            }
        }
    }
    assert!(count > 0.0, "{block} not found");
    sum.map(|value| value / count)
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

#[test]
fn pack_id_renames_the_dimension_data_pack() {
    let temporary = tempfile::tempdir().unwrap();
    let source = temporary.path().join("labels.nii");
    let labels = vec![1u8; 64];
    write_fixture(
        &source,
        &Fixture {
            dimensions: [4, 4, 4],
            spacing: [1.0; 3],
            sform: None,
            xml: "",
            ecode: 0,
        },
        &labels,
    );
    let world = temporary.path().join("world");
    create_world_with(
        &source,
        &world,
        &WorldOptions {
            pack_id: Some("modelmyscan".to_string()),
            ..WorldOptions::default()
        },
    )
    .unwrap();
    assert!(
        world
            .join("datapacks/modelmyscan/data/modelmyscan/dimension_type/overworld.json")
            .is_file()
    );
    assert!(!world.join("datapacks/nii2mc").exists());
    let mut level = Vec::new();
    GzDecoder::new(fs::File::open(world.join("level.dat")).unwrap())
        .read_to_end(&mut level)
        .unwrap();
    let text = String::from_utf8_lossy(&level);
    assert!(text.contains("file/modelmyscan"));
    assert!(!text.contains("nii2mc"));
    let mut settings = Vec::new();
    GzDecoder::new(fs::File::open(world.join("data/minecraft/world_gen_settings.dat")).unwrap())
        .read_to_end(&mut settings)
        .unwrap();
    assert!(String::from_utf8_lossy(&settings).contains("modelmyscan:overworld"));
    let pack = fs::read_to_string(world.join("datapacks/modelmyscan/pack.mcmeta")).unwrap();
    assert!(!pack.contains("nii2mc"));

    Command::cargo_bin("nii2mc")
        .unwrap()
        .args(["to-world", source.to_str().unwrap(), "--output"])
        .arg(temporary.path().join("bad"))
        .args(["--pack-id", "Bad Name"])
        .assert()
        .failure();
}
