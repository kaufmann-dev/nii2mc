//! Optional isotropic, label-aware resampling and anatomical orientation.
//!
//! Without these options nii2mc keeps its default contract: one NIfTI voxel is
//! one Minecraft block and axes are neither flipped nor resampled. Medical
//! scans often have anisotropic voxels (for example 0.7 x 0.7 x 5 mm), which
//! makes a one-voxel-per-block world look squashed, and their voxel axes can be
//! a mirror image of anatomy once mapped onto Minecraft's axes. The functions
//! here build a new label grid of cubic blocks and a matching NIfTI header, so
//! the rest of the pipeline (and `to-nifti`) treats the result as the input.

// Small fixed-size matrix code reads more clearly with explicit indices.
#![allow(clippy::needless_range_loop)]

use crate::error::{AppError, Result};
use crate::nifti::{Endian, NiftiVolume, f32_at, i16_at, metadata_from_prefix};
use clap::ValueEnum;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::str::FromStr;

/// Largest supersampling factor per axis when a block covers many voxels.
const MAX_SUPERSAMPLING: usize = 6;
/// Automatic block sizes, in millimetres, tried from finest to coarsest.
const AUTO_BLOCK_LADDER_MM: [f64; 17] = [
    0.25, 0.5, 0.75, 1.0, 1.25, 1.5, 2.0, 2.5, 3.0, 4.0, 5.0, 6.0, 8.0, 10.0, 12.0, 16.0, 20.0,
];
/// Automatic block size keeps the vertical axis at or below this many blocks.
pub const AUTO_MAX_VERTICAL_BLOCKS: f64 = 320.0;
/// Automatic block size keeps the whole grid at or below this many blocks.
pub const AUTO_MAX_BLOCKS: f64 = 64_000_000.0;

type Affine = [[f64; 4]; 3];

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Orientation {
    /// Keep NIfTI voxel axes exactly as stored (default)
    Voxel,
    /// Superior points up and left/right is not mirrored, using the NIfTI sform/qform
    Anatomical,
}

impl Orientation {
    pub fn name(self) -> &'static str {
        match self {
            Self::Voxel => "voxel",
            Self::Anatomical => "anatomical",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BlockSize {
    Millimetres(f64),
    Auto,
}

impl FromStr for BlockSize {
    type Err = String;

    fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
        if value.eq_ignore_ascii_case("auto") {
            return Ok(Self::Auto);
        }
        let millimetres: f64 = value
            .parse()
            .map_err(|_| format!("expected a block size in mm or 'auto', got {value:?}"))?;
        if !millimetres.is_finite() || millimetres <= 0.0 {
            return Err("block size must be a positive number of millimetres".to_string());
        }
        Ok(Self::Millimetres(millimetres))
    }
}

#[derive(Debug, Clone, Copy)]
pub struct TransformOptions {
    pub block: Option<BlockSize>,
    pub orientation: Orientation,
    pub fill_threshold: f64,
    pub crop: bool,
}

impl Default for TransformOptions {
    fn default() -> Self {
        Self {
            block: None,
            orientation: Orientation::Voxel,
            fill_threshold: 0.5,
            crop: false,
        }
    }
}

impl TransformOptions {
    pub fn is_identity(&self) -> bool {
        self.block.is_none() && self.orientation == Orientation::Voxel && !self.crop
    }
}

/// How a world or resampled file was derived from its source NIfTI grid.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TransformRecord {
    pub orientation: String,
    pub block_mm: Option<f64>,
    pub fill_threshold: Option<f64>,
    pub cropped: bool,
    pub source_dimensions: [u32; 3],
    pub source_spacing: [f32; 3],
    pub dimensions: [u32; 3],
    pub affine: [[f64; 4]; 3],
}

/// Apply optional cropping, orientation, and isotropic resampling.
///
/// The returned volume carries a regenerated header (dimensions, pixdim,
/// sform and qform) followed by the original extension bytes, so embedded
/// label tables survive. Label values and the integer datatype never change.
pub fn transform(
    volume: NiftiVolume,
    options: &TransformOptions,
) -> Result<(NiftiVolume, TransformRecord)> {
    if !(options.fill_threshold > 0.0 && options.fill_threshold <= 1.0) {
        return Err(AppError::usage(
            "--fill-threshold must be greater than 0 and at most 1",
        ));
    }
    if options.block.is_none() && options.crop {
        return Err(AppError::usage("--crop requires --block-mm"));
    }
    let endian = volume.endian;
    let source = source_affine(&volume.prefix, endian, &volume)?;
    if options.orientation == Orientation::Anatomical && !has_orientation(&volume.prefix, endian) {
        return Err(AppError::incompatible(
            "--orient anatomical needs an sform or qform in the NIfTI header; this file has neither",
        ));
    }
    let source_dimensions = volume.metadata.dimensions;
    let source_spacing = volume.metadata.spacing;

    let (dimensions, affine, voxels, block_mm) = match options.block {
        None if options.orientation == Orientation::Voxel => {
            return Err(AppError::usage(
                "nothing to transform: pass --block-mm and/or --orient anatomical",
            ));
        }
        None => {
            let (dimensions, affine, voxels) = permute_to_anatomy(&volume, &source);
            (dimensions, affine, voxels, None)
        }
        Some(block) => {
            let frame = match options.orientation {
                Orientation::Anatomical => ANATOMICAL_FRAME,
                Orientation::Voxel => voxel_frame(&source),
            };
            let (low, high) = source_bounds(&volume, options.crop)?;
            let extent = frame_extent(&source, &frame, low, high);
            let block_mm = match block {
                BlockSize::Millimetres(value) => value,
                BlockSize::Auto => auto_block_size(&volume, &extent, options.orientation),
            };
            let (dimensions, affine) = target_grid(&frame, &extent, block_mm)?;
            let voxels = resample_labels(&volume, &source, &affine, dimensions, options)?;
            (dimensions, affine, voxels, Some(block_mm))
        }
    };

    let prefix = regenerate_prefix(&volume.prefix, endian, dimensions, &affine)?;
    let mut counts = BTreeMap::new();
    for label in &voxels {
        *counts.entry(*label).or_insert(0u64) += 1;
    }
    let metadata = metadata_from_prefix(
        &prefix,
        endian,
        dimensions,
        volume.metadata.datatype,
        volume.metadata.bits_per_voxel,
    );
    let record = TransformRecord {
        orientation: options.orientation.name().to_string(),
        block_mm,
        fill_threshold: block_mm.map(|_| options.fill_threshold),
        cropped: options.crop,
        source_dimensions,
        source_spacing,
        dimensions,
        affine,
    };
    let transformed = NiftiVolume {
        metadata,
        prefix,
        voxels,
        counts,
        label_names: volume.label_names,
        source_sha256: volume.source_sha256,
        endian,
    };
    Ok((transformed, record))
}

// --------------------------------------------------------------- affines
fn has_orientation(prefix: &[u8], endian: Endian) -> bool {
    i16_at(prefix, 254, endian) > 0 || i16_at(prefix, 252, endian) > 0
}

/// Voxel index -> physical millimetres, preferring the sform like most readers.
fn source_affine(prefix: &[u8], endian: Endian, volume: &NiftiVolume) -> Result<Affine> {
    let pixdim = |offset| f64::from(f32_at(prefix, offset, endian));
    let affine = if i16_at(prefix, 254, endian) > 0 {
        let row = |offset: usize| {
            [
                pixdim(offset),
                pixdim(offset + 4),
                pixdim(offset + 8),
                pixdim(offset + 12),
            ]
        };
        [row(280), row(296), row(312)]
    } else if i16_at(prefix, 252, endian) > 0 {
        let (b, c, d) = (pixdim(256), pixdim(260), pixdim(264));
        let a = (1.0 - (b * b + c * c + d * d)).max(0.0).sqrt();
        let rotation = [
            [
                a * a + b * b - c * c - d * d,
                2.0 * (b * c - a * d),
                2.0 * (b * d + a * c),
            ],
            [
                2.0 * (b * c + a * d),
                a * a + c * c - b * b - d * d,
                2.0 * (c * d - a * b),
            ],
            [
                2.0 * (b * d - a * c),
                2.0 * (c * d + a * b),
                a * a + d * d - b * b - c * c,
            ],
        ];
        let qfac = if pixdim(76) < 0.0 { -1.0 } else { 1.0 };
        let scale = [pixdim(80).abs(), pixdim(84).abs(), pixdim(88).abs() * qfac];
        let offset = [pixdim(268), pixdim(272), pixdim(276)];
        let mut affine = [[0.0; 4]; 3];
        for row in 0..3 {
            for column in 0..3 {
                affine[row][column] = rotation[row][column] * scale[column];
            }
            affine[row][3] = offset[row];
        }
        affine
    } else {
        let spacing = volume.metadata.spacing;
        [
            [f64::from(spacing[0]), 0.0, 0.0, 0.0],
            [0.0, f64::from(spacing[1]), 0.0, 0.0],
            [0.0, 0.0, f64::from(spacing[2]), 0.0],
        ]
    };
    let linear = linear_part(&affine);
    if !affine.iter().flatten().all(|value| value.is_finite()) || determinant(&linear).abs() < 1e-12
    {
        return Err(AppError::incompatible(
            "the NIfTI orientation matrix is singular or not finite",
        ));
    }
    Ok(affine)
}

fn linear_part(affine: &Affine) -> [[f64; 3]; 3] {
    [
        [affine[0][0], affine[0][1], affine[0][2]],
        [affine[1][0], affine[1][1], affine[1][2]],
        [affine[2][0], affine[2][1], affine[2][2]],
    ]
}

fn determinant(m: &[[f64; 3]; 3]) -> f64 {
    m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
        - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
        + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0])
}

fn invert(affine: &Affine) -> Affine {
    let m = linear_part(affine);
    let det = determinant(&m);
    let mut inverse = [[0.0; 3]; 3];
    inverse[0][0] = (m[1][1] * m[2][2] - m[1][2] * m[2][1]) / det;
    inverse[0][1] = (m[0][2] * m[2][1] - m[0][1] * m[2][2]) / det;
    inverse[0][2] = (m[0][1] * m[1][2] - m[0][2] * m[1][1]) / det;
    inverse[1][0] = (m[1][2] * m[2][0] - m[1][0] * m[2][2]) / det;
    inverse[1][1] = (m[0][0] * m[2][2] - m[0][2] * m[2][0]) / det;
    inverse[1][2] = (m[0][2] * m[1][0] - m[0][0] * m[1][2]) / det;
    inverse[2][0] = (m[1][0] * m[2][1] - m[1][1] * m[2][0]) / det;
    inverse[2][1] = (m[0][1] * m[2][0] - m[0][0] * m[2][1]) / det;
    inverse[2][2] = (m[0][0] * m[1][1] - m[0][1] * m[1][0]) / det;
    let mut result = [[0.0; 4]; 3];
    for row in 0..3 {
        for column in 0..3 {
            result[row][column] = inverse[row][column];
        }
        result[row][3] = -(0..3).map(|k| inverse[row][k] * affine[k][3]).sum::<f64>();
    }
    result
}

fn apply(affine: &Affine, point: [f64; 3]) -> [f64; 3] {
    let mut out = [0.0; 3];
    for (row, value) in out.iter_mut().enumerate() {
        *value = affine[row][0] * point[0]
            + affine[row][1] * point[1]
            + affine[row][2] * point[2]
            + affine[row][3];
    }
    out
}

fn compose(outer: &Affine, inner: &Affine) -> Affine {
    let mut result = [[0.0; 4]; 3];
    for row in 0..3 {
        for column in 0..4 {
            let mut value = if column == 3 { outer[row][3] } else { 0.0 };
            for k in 0..3 {
                value += outer[row][k] * inner[k][column];
            }
            result[row][column] = value;
        }
    }
    result
}

/// Target index axes for anatomical output: +i = Right, +j = Posterior,
/// +k = Superior (NIfTI RAS+ physical space). nii2mc places i on Minecraft X,
/// j on Z, and k on Y, so the model stands upright and is not mirrored.
const ANATOMICAL_FRAME: [[f64; 3]; 3] = [[1.0, 0.0, 0.0], [0.0, -1.0, 0.0], [0.0, 0.0, 1.0]];

/// Target axes following the source voxel axes (columns are unit directions).
fn voxel_frame(source: &Affine) -> [[f64; 3]; 3] {
    let mut frame = [[0.0; 3]; 3];
    for column in 0..3 {
        let length = (0..3)
            .map(|row| source[row][column] * source[row][column])
            .sum::<f64>()
            .sqrt();
        for row in 0..3 {
            frame[row][column] = source[row][column] / length;
        }
    }
    frame
}

/// Continuous source index box to cover: the whole volume, or the labelled
/// bounding box plus one voxel when cropping.
fn source_bounds(volume: &NiftiVolume, crop: bool) -> Result<([f64; 3], [f64; 3])> {
    let dims = volume.metadata.dimensions.map(|value| value as usize);
    if !crop {
        return Ok((
            [-0.5; 3],
            [
                dims[0] as f64 - 0.5,
                dims[1] as f64 - 0.5,
                dims[2] as f64 - 0.5,
            ],
        ));
    }
    let mut low = [usize::MAX; 3];
    let mut high = [0usize; 3];
    let mut found = false;
    for k in 0..dims[2] {
        for j in 0..dims[1] {
            let row = (k * dims[1] + j) * dims[0];
            for i in 0..dims[0] {
                if volume.voxels[row + i] != 0 {
                    found = true;
                    for (axis, index) in [i, j, k].into_iter().enumerate() {
                        low[axis] = low[axis].min(index);
                        high[axis] = high[axis].max(index);
                    }
                }
            }
        }
    }
    if !found {
        return Err(AppError::incompatible(
            "--crop found no nonzero labels to crop to",
        ));
    }
    Ok((
        low.map(|value| value as f64 - 1.5),
        high.map(|value| value as f64 + 1.5),
    ))
}

/// Bounds of the source box expressed along the target frame axes.
fn frame_extent(
    source: &Affine,
    frame: &[[f64; 3]; 3],
    low: [f64; 3],
    high: [f64; 3],
) -> ([f64; 3], [f64; 3]) {
    let mut minimum = [f64::INFINITY; 3];
    let mut maximum = [f64::NEG_INFINITY; 3];
    for corner in 0..8 {
        let index = [
            if corner & 1 == 0 { low[0] } else { high[0] },
            if corner & 2 == 0 { low[1] } else { high[1] },
            if corner & 4 == 0 { low[2] } else { high[2] },
        ];
        let point = apply(source, index);
        for axis in 0..3 {
            let coordinate = (0..3).map(|row| frame[row][axis] * point[row]).sum::<f64>();
            minimum[axis] = minimum[axis].min(coordinate);
            maximum[axis] = maximum[axis].max(coordinate);
        }
    }
    (minimum, maximum)
}

fn auto_block_size(
    volume: &NiftiVolume,
    extent: &([f64; 3], [f64; 3]),
    orientation: Orientation,
) -> f64 {
    let finest = volume
        .metadata
        .spacing
        .iter()
        .map(|value| f64::from(*value))
        .fold(f64::INFINITY, f64::min);
    let lengths = [
        extent.1[0] - extent.0[0],
        extent.1[1] - extent.0[1],
        extent.1[2] - extent.0[2],
    ];
    // Anatomical output is always vertical along k; voxel output uses the
    // default vertical axis z (k) unless the caller chose another axis, in
    // which case the tallest axis is a safe bound.
    let vertical = match orientation {
        Orientation::Anatomical => lengths[2],
        Orientation::Voxel => lengths.iter().copied().fold(0.0, f64::max),
    };
    for candidate in AUTO_BLOCK_LADDER_MM {
        if candidate + 1e-9 < finest {
            continue;
        }
        let blocks = lengths
            .iter()
            .map(|length| (length / candidate).ceil())
            .product::<f64>();
        if vertical / candidate <= AUTO_MAX_VERTICAL_BLOCKS && blocks <= AUTO_MAX_BLOCKS {
            return candidate;
        }
    }
    AUTO_BLOCK_LADDER_MM[AUTO_BLOCK_LADDER_MM.len() - 1]
}

fn target_grid(
    frame: &[[f64; 3]; 3],
    extent: &([f64; 3], [f64; 3]),
    block_mm: f64,
) -> Result<([u32; 3], Affine)> {
    let mut dimensions = [0u32; 3];
    let mut first_center = [0.0; 3];
    for axis in 0..3 {
        let length = extent.1[axis] - extent.0[axis];
        let count = ((length / block_mm) - 1e-6).ceil().max(1.0);
        if count > f64::from(i16::MAX) {
            return Err(AppError::incompatible(format!(
                "a {block_mm} mm block grid needs {count} blocks on one axis; NIfTI-1 allows at most {}",
                i16::MAX
            )));
        }
        dimensions[axis] = count as u32;
        first_center[axis] = extent.0[axis] + (length - count * block_mm) / 2.0 + block_mm / 2.0;
    }
    let mut affine = [[0.0; 4]; 3];
    for row in 0..3 {
        for column in 0..3 {
            affine[row][column] = frame[row][column] * block_mm;
        }
        affine[row][3] = (0..3)
            .map(|axis| frame[row][axis] * first_center[axis])
            .sum();
    }
    Ok((dimensions, affine))
}

// ------------------------------------------------------------- resampling
fn resample_labels(
    volume: &NiftiVolume,
    source: &Affine,
    target: &Affine,
    dimensions: [u32; 3],
    options: &TransformOptions,
) -> Result<Vec<u32>> {
    let to_source = compose(&invert(source), target);
    let source_dims = volume.metadata.dimensions.map(|value| value as i64);
    // Steps (in source voxels) of one target block along each target axis.
    let step = |axis: usize| {
        (0..3)
            .map(|row| to_source[row][axis] * to_source[row][axis])
            .sum::<f64>()
            .sqrt()
    };
    let supersampling: [usize; 3] =
        [0, 1, 2].map(|axis| (step(axis).ceil() as usize).clamp(1, MAX_SUPERSAMPLING));
    let samples = supersampling.iter().product::<usize>() as f64;
    let offsets: Vec<[f64; 3]> = {
        let mut values = Vec::with_capacity(samples as usize);
        for c in 0..supersampling[2] {
            for b in 0..supersampling[1] {
                for a in 0..supersampling[0] {
                    values.push([
                        (a as f64 + 0.5) / supersampling[0] as f64 - 0.5,
                        (b as f64 + 0.5) / supersampling[1] as f64 - 0.5,
                        (c as f64 + 0.5) / supersampling[2] as f64 - 0.5,
                    ]);
                }
            }
        }
        values
    };
    let target_dims = dimensions.map(|value| value as usize);
    let total = target_dims[0]
        .checked_mul(target_dims[1])
        .and_then(|value| value.checked_mul(target_dims[2]))
        .ok_or_else(|| AppError::incompatible("block grid dimensions overflow this platform"))?;
    let mut output = vec![0u32; total];
    let slice_len = target_dims[0] * target_dims[1];
    let threads = std::thread::available_parallelism()
        .map(|value| value.get())
        .unwrap_or(1)
        .min(target_dims[2].max(1));
    let slices_per_thread = target_dims[2].div_ceil(threads.max(1)).max(1);
    let threshold = options.fill_threshold;
    let voxels = &volume.voxels;

    std::thread::scope(|scope| {
        for (chunk_index, chunk) in output.chunks_mut(slice_len * slices_per_thread).enumerate() {
            let offsets = &offsets;
            scope.spawn(move || {
                let mut weights: Vec<(u32, f64)> = Vec::with_capacity(16);
                let first_k = chunk_index * slices_per_thread;
                for (local, value) in chunk.iter_mut().enumerate() {
                    let k = first_k + local / slice_len;
                    let j = (local % slice_len) / target_dims[0];
                    let i = local % target_dims[0];
                    weights.clear();
                    for offset in offsets {
                        let point = [
                            i as f64 + offset[0],
                            j as f64 + offset[1],
                            k as f64 + offset[2],
                        ];
                        let s = apply(&to_source, point);
                        accumulate_trilinear(voxels, source_dims, s, &mut weights);
                    }
                    *value = decide(&weights, samples, threshold);
                }
            });
        }
    });
    Ok(output)
}

fn accumulate_trilinear(
    voxels: &[u32],
    dims: [i64; 3],
    position: [f64; 3],
    weights: &mut Vec<(u32, f64)>,
) {
    let base = position.map(f64::floor);
    let fraction = [
        position[0] - base[0],
        position[1] - base[1],
        position[2] - base[2],
    ];
    let base = base.map(|value| value as i64);
    for corner in 0..8 {
        let offset = [corner & 1, (corner >> 1) & 1, (corner >> 2) & 1];
        let mut weight = 1.0;
        let mut index = [0i64; 3];
        for axis in 0..3 {
            index[axis] = base[axis] + offset[axis] as i64;
            weight *= if offset[axis] == 1 {
                fraction[axis]
            } else {
                1.0 - fraction[axis]
            };
        }
        if weight <= 0.0 {
            continue;
        }
        if (0..3).any(|axis| index[axis] < 0 || index[axis] >= dims[axis]) {
            continue;
        }
        let flat = (index[0] + dims[0] * (index[1] + dims[1] * index[2])) as usize;
        let label = voxels[flat];
        if label == 0 {
            continue;
        }
        if let Some(entry) = weights.iter_mut().find(|(existing, _)| *existing == label) {
            entry.1 += weight;
        } else {
            weights.push((label, weight));
        }
    }
}

/// A block is filled when labelled coverage reaches the threshold; it then
/// takes the label covering the most of it (ties go to the smaller label).
fn decide(weights: &[(u32, f64)], samples: f64, threshold: f64) -> u32 {
    let foreground: f64 = weights.iter().map(|(_, weight)| weight).sum();
    if foreground / samples + 1e-9 < threshold {
        return 0;
    }
    let mut best = (0u32, f64::NEG_INFINITY);
    for &(label, weight) in weights {
        if weight > best.1 + 1e-12 || ((weight - best.1).abs() <= 1e-12 && label < best.0) {
            best = (label, weight);
        }
    }
    best.0
}

/// Without resampling, anatomical orientation only permutes and flips voxel
/// axes, so every voxel still becomes exactly one block.
fn permute_to_anatomy(volume: &NiftiVolume, source: &Affine) -> ([u32; 3], Affine, Vec<u32>) {
    let frame = ANATOMICAL_FRAME;
    // For each target axis pick the source axis whose direction matches best.
    let directions = voxel_frame(source);
    let mut used = [false; 3];
    let mut mapping = [(0usize, 1.0f64); 3];
    for target_axis in 0..3 {
        let mut best = (usize::MAX, 0.0f64);
        for (source_axis, taken) in used.iter().enumerate() {
            if *taken {
                continue;
            }
            let dot: f64 = (0..3)
                .map(|row| directions[row][source_axis] * frame[row][target_axis])
                .sum();
            if best.0 == usize::MAX || dot.abs() > best.1.abs() {
                best = (source_axis, dot);
            }
        }
        used[best.0] = true;
        mapping[target_axis] = (best.0, if best.1 < 0.0 { -1.0 } else { 1.0 });
    }
    let source_dims = volume.metadata.dimensions;
    let dimensions = [
        source_dims[mapping[0].0],
        source_dims[mapping[1].0],
        source_dims[mapping[2].0],
    ];
    // Target index t -> source index: s[mapping[a].0] = t[a] or n-1-t[a].
    let mut index_map = [[0.0; 4]; 3];
    for (target_axis, (source_axis, sign)) in mapping.iter().enumerate() {
        index_map[*source_axis][target_axis] = *sign;
        if *sign < 0.0 {
            index_map[*source_axis][3] = f64::from(source_dims[*source_axis]) - 1.0;
        }
    }
    let affine = compose(source, &index_map);
    let n = dimensions.map(|value| value as usize);
    let sn = source_dims.map(|value| value as usize);
    let mut voxels = vec![0u32; n[0] * n[1] * n[2]];
    for k in 0..n[2] {
        for j in 0..n[1] {
            for i in 0..n[0] {
                let t = [i, j, k];
                let mut s = [0usize; 3];
                for (target_axis, (source_axis, sign)) in mapping.iter().enumerate() {
                    s[*source_axis] = if *sign < 0.0 {
                        sn[*source_axis] - 1 - t[target_axis]
                    } else {
                        t[target_axis]
                    };
                }
                voxels[i + n[0] * (j + n[1] * k)] =
                    volume.voxels[s[0] + sn[0] * (s[1] + sn[1] * s[2])];
            }
        }
    }
    (dimensions, affine, voxels)
}

// ------------------------------------------------------------- header
fn put_i16(bytes: &mut [u8], offset: usize, value: i16, endian: Endian) {
    let encoded = match endian {
        Endian::Little => value.to_le_bytes(),
        Endian::Big => value.to_be_bytes(),
    };
    bytes[offset..offset + 2].copy_from_slice(&encoded);
}

fn put_f32(bytes: &mut [u8], offset: usize, value: f32, endian: Endian) {
    let encoded = match endian {
        Endian::Little => value.to_le_bytes(),
        Endian::Big => value.to_be_bytes(),
    };
    bytes[offset..offset + 4].copy_from_slice(&encoded);
}

/// Copy the original header and extensions, replacing only the geometry.
fn regenerate_prefix(
    prefix: &[u8],
    endian: Endian,
    dimensions: [u32; 3],
    affine: &Affine,
) -> Result<Vec<u8>> {
    let mut header = prefix.to_vec();
    for (axis, value) in dimensions.iter().enumerate() {
        let value = i16::try_from(*value).map_err(|_| {
            AppError::incompatible("block grid exceeds the NIfTI-1 dimension limit")
        })?;
        put_i16(&mut header, 42 + 2 * axis, value, endian);
    }
    let mut spacing = [0.0f64; 3];
    for (column, value) in spacing.iter_mut().enumerate() {
        *value = (0..3)
            .map(|row| affine[row][column] * affine[row][column])
            .sum::<f64>()
            .sqrt();
    }
    let mut rotation = [[0.0; 3]; 3];
    for row in 0..3 {
        for column in 0..3 {
            rotation[row][column] = affine[row][column] / spacing[column];
        }
    }
    let qfac = if determinant(&rotation) < 0.0 {
        for row in rotation.iter_mut() {
            row[2] = -row[2];
        }
        -1.0f32
    } else {
        1.0f32
    };
    let quaternion = rotation_to_quaternion(&rotation);
    put_f32(&mut header, 76, qfac, endian);
    for (axis, value) in spacing.iter().enumerate() {
        put_f32(&mut header, 80 + 4 * axis, *value as f32, endian);
    }
    let original_sform = i16_at(prefix, 254, endian);
    let original_qform = i16_at(prefix, 252, endian);
    let code = if original_sform > 0 {
        original_sform
    } else if original_qform > 0 {
        original_qform
    } else {
        1
    };
    put_i16(
        &mut header,
        252,
        if original_qform > 0 {
            original_qform
        } else {
            code
        },
        endian,
    );
    put_i16(&mut header, 254, code, endian);
    for (index, value) in quaternion.iter().enumerate() {
        put_f32(&mut header, 256 + 4 * index, *value as f32, endian);
    }
    for row in 0..3 {
        put_f32(&mut header, 268 + 4 * row, affine[row][3] as f32, endian);
        for column in 0..4 {
            put_f32(
                &mut header,
                280 + 16 * row + 4 * column,
                affine[row][column] as f32,
                endian,
            );
        }
    }
    Ok(header)
}

/// Quaternion (b, c, d) of a proper rotation, following the NIfTI-1 reference.
fn rotation_to_quaternion(r: &[[f64; 3]; 3]) -> [f64; 3] {
    let trace = r[0][0] + r[1][1] + r[2][2] + 1.0;
    let (mut a, mut b, mut c, mut d);
    if trace > 0.5 {
        a = 0.5 * trace.sqrt();
        b = 0.25 * (r[2][1] - r[1][2]) / a;
        c = 0.25 * (r[0][2] - r[2][0]) / a;
        d = 0.25 * (r[1][0] - r[0][1]) / a;
    } else {
        let xd = 1.0 + r[0][0] - (r[1][1] + r[2][2]);
        let yd = 1.0 + r[1][1] - (r[0][0] + r[2][2]);
        let zd = 1.0 + r[2][2] - (r[0][0] + r[1][1]);
        if xd > 1.0 {
            b = 0.5 * xd.sqrt();
            c = 0.25 * (r[0][1] + r[1][0]) / b;
            d = 0.25 * (r[0][2] + r[2][0]) / b;
            a = 0.25 * (r[2][1] - r[1][2]) / b;
        } else if yd > 1.0 {
            c = 0.5 * yd.sqrt();
            b = 0.25 * (r[0][1] + r[1][0]) / c;
            d = 0.25 * (r[1][2] + r[2][1]) / c;
            a = 0.25 * (r[0][2] - r[2][0]) / c;
        } else {
            d = 0.5 * zd.sqrt();
            b = 0.25 * (r[0][2] + r[2][0]) / d;
            c = 0.25 * (r[1][2] + r[2][1]) / d;
            a = 0.25 * (r[1][0] - r[0][1]) / d;
        }
        if a < 0.0 {
            b = -b;
            c = -c;
            d = -d;
            a = -a;
        }
    }
    let _ = a;
    [b, c, d]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_size_parses_millimetres_and_auto() {
        assert_eq!("2".parse::<BlockSize>(), Ok(BlockSize::Millimetres(2.0)));
        assert_eq!("AUTO".parse::<BlockSize>(), Ok(BlockSize::Auto));
        assert!("0".parse::<BlockSize>().is_err());
        assert!("-1".parse::<BlockSize>().is_err());
        assert!("x".parse::<BlockSize>().is_err());
    }

    #[test]
    fn decide_fills_by_coverage_and_prefers_the_largest_label() {
        assert_eq!(decide(&[(3, 0.3), (5, 0.25)], 1.0, 0.5), 3);
        assert_eq!(decide(&[(3, 0.3)], 1.0, 0.5), 0);
        assert_eq!(decide(&[(7, 0.3), (2, 0.3)], 1.0, 0.5), 2);
    }

    #[test]
    fn quaternion_round_trips_simple_rotations() {
        let flip_yz = [[1.0, 0.0, 0.0], [0.0, -1.0, 0.0], [0.0, 0.0, -1.0]];
        let q = rotation_to_quaternion(&flip_yz);
        assert!((q[0] - 1.0).abs() < 1e-9 && q[1].abs() < 1e-9 && q[2].abs() < 1e-9);
        let identity = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
        assert_eq!(rotation_to_quaternion(&identity), [0.0, 0.0, 0.0]);
    }

    #[test]
    fn affine_inverse_composes_to_identity() {
        let affine = [
            [0.0, -2.0, 0.0, 5.0],
            [0.5, 0.0, 0.0, -1.0],
            [0.0, 0.0, 3.0, 2.0],
        ];
        let identity = compose(&invert(&affine), &affine);
        for row in 0..3 {
            for column in 0..4 {
                let expected = if row == column { 1.0 } else { 0.0 };
                assert!((identity[row][column] - expected).abs() < 1e-12);
            }
        }
    }
}
