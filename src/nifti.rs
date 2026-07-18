use crate::error::{AppError, ErrorKind, Result};
use crate::manifest::NiftiMetadata;
use flate2::Compression;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use quick_xml::Reader as XmlReader;
use quick_xml::events::Event;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::Path;

const NIFTI1_HEADER_SIZE: usize = 348;
const NIFTI1_MIN_OFFSET: usize = 352;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Endian {
    Little,
    Big,
}

impl Endian {
    fn name(self) -> &'static str {
        match self {
            Self::Little => "little",
            Self::Big => "big",
        }
    }
}

#[derive(Debug)]
pub struct NiftiVolume {
    pub metadata: NiftiMetadata,
    pub prefix: Vec<u8>,
    pub voxels: Vec<u32>,
    pub counts: BTreeMap<u32, u64>,
    pub label_names: BTreeMap<u32, String>,
    pub source_sha256: String,
    pub endian: Endian,
}

#[derive(Debug, Serialize)]
pub struct NiftiInspection {
    pub kind: &'static str,
    pub dimensions: [u32; 3],
    pub spacing: [f32; 3],
    pub datatype: String,
    pub endianness: String,
    pub voxel_count: u64,
    pub background_voxels: u64,
    pub nonzero_labels: usize,
    pub labels: Vec<LabelInspection>,
    pub embedded_label_names: bool,
    pub sha256: String,
}

#[derive(Debug, Serialize)]
pub struct LabelInspection {
    pub label: u32,
    pub name: Option<String>,
    pub voxel_count: u64,
}

impl NiftiVolume {
    pub fn inspection(&self) -> NiftiInspection {
        let labels = self
            .counts
            .iter()
            .filter(|(label, _)| **label != 0)
            .map(|(label, count)| LabelInspection {
                label: *label,
                name: self.label_names.get(label).cloned(),
                voxel_count: *count,
            })
            .collect();
        NiftiInspection {
            kind: "nifti",
            dimensions: self.metadata.dimensions,
            spacing: self.metadata.spacing,
            datatype: datatype_name(self.metadata.datatype).to_string(),
            endianness: self.metadata.endianness.clone(),
            voxel_count: self.voxels.len() as u64,
            background_voxels: self.counts.get(&0).copied().unwrap_or(0),
            nonzero_labels: self.counts.keys().filter(|label| **label != 0).count(),
            labels,
            embedded_label_names: !self.label_names.is_empty(),
            sha256: self.source_sha256.clone(),
        }
    }
}

pub fn read_nifti(path: &Path) -> Result<NiftiVolume> {
    validate_nifti_path(path)?;
    let source_sha256 = sha256_file(path)?;
    let file = File::open(path)
        .map_err(|error| AppError::io(format!("cannot open {}: {error}", path.display())))?;
    let mut magic = [0u8; 2];
    let mut magic_reader = BufReader::new(file);
    let magic_len = magic_reader.read(&mut magic)?;
    drop(magic_reader);

    let file = File::open(path)?;
    let reader: Box<dyn Read> = if magic_len == 2 && magic == [0x1f, 0x8b] {
        Box::new(GzDecoder::new(file))
    } else {
        Box::new(file)
    };
    parse_uncompressed(BufReader::new(reader), source_sha256)
}

fn parse_uncompressed(
    mut reader: BufReader<Box<dyn Read>>,
    source_sha256: String,
) -> Result<NiftiVolume> {
    let mut header = vec![0u8; NIFTI1_HEADER_SIZE];
    reader
        .read_exact(&mut header)
        .map_err(|error| AppError::incompatible(format!("cannot read NIfTI-1 header: {error}")))?;
    let endian = detect_endian(&header)?;
    if &header[344..348] != b"n+1\0" {
        return Err(AppError::incompatible(
            "input is not a single-file NIfTI-1 image (expected n+1 magic)",
        ));
    }

    let dim_count = i16_at(&header, 40, endian);
    if dim_count != 3 {
        return Err(AppError::incompatible(format!(
            "expected a 3D NIfTI label map, but dim[0] is {dim_count}"
        )));
    }
    let dimensions = [
        positive_dimension(i16_at(&header, 42, endian), 1)?,
        positive_dimension(i16_at(&header, 44, endian), 2)?,
        positive_dimension(i16_at(&header, 46, endian), 3)?,
    ];
    let datatype = i16_at(&header, 70, endian);
    if datatype < 0 {
        return Err(AppError::incompatible("negative NIfTI datatype code"));
    }
    let datatype = datatype as u16;
    let expected_bits = datatype_bits(datatype)?;
    let bits_per_voxel = i16_at(&header, 72, endian);
    if bits_per_voxel != expected_bits as i16 {
        return Err(AppError::incompatible(format!(
            "datatype {} requires bitpix {}, but header contains {}",
            datatype_name(datatype),
            expected_bits,
            bits_per_voxel
        )));
    }
    let slope = f32_at(&header, 112, endian);
    let intercept = f32_at(&header, 116, endian);
    if !((slope == 0.0 || slope == 1.0) && intercept == 0.0) {
        return Err(AppError::incompatible(format!(
            "scaled label maps are not supported (scl_slope={slope}, scl_inter={intercept})"
        )));
    }

    let voxel_offset_f = f32_at(&header, 108, endian);
    if !voxel_offset_f.is_finite()
        || voxel_offset_f < NIFTI1_MIN_OFFSET as f32
        || voxel_offset_f.fract() != 0.0
    {
        return Err(AppError::incompatible(format!(
            "invalid NIfTI voxel offset {voxel_offset_f}"
        )));
    }
    let voxel_offset = voxel_offset_f as usize;
    let mut prefix = header;
    prefix.resize(voxel_offset, 0);
    reader
        .read_exact(&mut prefix[NIFTI1_HEADER_SIZE..])
        .map_err(|error| {
            AppError::incompatible(format!("truncated NIfTI extension area: {error}"))
        })?;

    let voxel_count = dimensions
        .iter()
        .try_fold(1usize, |total, dimension| {
            total.checked_mul(*dimension as usize)
        })
        .ok_or_else(|| AppError::incompatible("NIfTI dimensions overflow this platform"))?;
    let bytes_per_voxel = expected_bits as usize / 8;
    let voxels = read_voxels(&mut reader, voxel_count, bytes_per_voxel, datatype, endian)?;
    let mut counts = BTreeMap::new();
    for label in &voxels {
        *counts.entry(*label).or_insert(0) += 1;
    }
    let label_names = parse_label_names(&prefix, endian);
    let units = prefix[123];
    let metadata = NiftiMetadata {
        dimensions,
        spacing: [
            f32_at(&prefix, 80, endian).abs(),
            f32_at(&prefix, 84, endian).abs(),
            f32_at(&prefix, 88, endian).abs(),
        ],
        datatype,
        bits_per_voxel: expected_bits,
        endianness: endian.name().to_string(),
        voxel_offset: voxel_offset as u64,
        qform_code: i16_at(&prefix, 252, endian),
        sform_code: i16_at(&prefix, 254, endian),
        quaternion: [
            f32_at(&prefix, 256, endian),
            f32_at(&prefix, 260, endian),
            f32_at(&prefix, 264, endian),
        ],
        qoffset: [
            f32_at(&prefix, 268, endian),
            f32_at(&prefix, 272, endian),
            f32_at(&prefix, 276, endian),
        ],
        srow_x: four_f32(&prefix, 280, endian),
        srow_y: four_f32(&prefix, 296, endian),
        srow_z: four_f32(&prefix, 312, endian),
        spatial_units: units & 0x07,
        temporal_units: units & 0x38,
        description: nul_terminated_text(&prefix[148..228]),
    };

    Ok(NiftiVolume {
        metadata,
        prefix,
        voxels,
        counts,
        label_names,
        source_sha256,
        endian,
    })
}

fn read_voxels(
    reader: &mut dyn Read,
    voxel_count: usize,
    bytes_per_voxel: usize,
    datatype: u16,
    endian: Endian,
) -> Result<Vec<u32>> {
    let mut voxels = Vec::with_capacity(voxel_count);
    let voxels_per_chunk = (1024 * 1024 / bytes_per_voxel).max(1);
    let mut remaining = voxel_count;
    let mut buffer = vec![0u8; voxels_per_chunk * bytes_per_voxel];
    while remaining > 0 {
        let count = remaining.min(voxels_per_chunk);
        let bytes = count * bytes_per_voxel;
        reader.read_exact(&mut buffer[..bytes]).map_err(|error| {
            AppError::incompatible(format!("truncated NIfTI voxel payload: {error}"))
        })?;
        for raw in buffer[..bytes].chunks_exact(bytes_per_voxel) {
            voxels.push(decode_label(raw, datatype, endian)?);
        }
        remaining -= count;
    }
    Ok(voxels)
}

fn decode_label(raw: &[u8], datatype: u16, endian: Endian) -> Result<u32> {
    let negative = |value: i64| {
        AppError::incompatible(format!(
            "label maps cannot contain negative values (found {value})"
        ))
    };
    match datatype {
        2 => Ok(raw[0] as u32),
        256 => {
            let value = raw[0] as i8 as i64;
            u32::try_from(value).map_err(|_| negative(value))
        }
        4 => {
            let value = read_i16(raw, endian) as i64;
            u32::try_from(value).map_err(|_| negative(value))
        }
        512 => Ok(read_u16(raw, endian) as u32),
        8 => {
            let value = read_i32(raw, endian) as i64;
            u32::try_from(value).map_err(|_| negative(value))
        }
        768 => Ok(read_u32(raw, endian)),
        _ => Err(AppError::incompatible(format!(
            "unsupported datatype code {datatype}"
        ))),
    }
}

pub fn write_nifti(
    path: &Path,
    prefix: &[u8],
    metadata: &NiftiMetadata,
    voxels: &[u32],
) -> Result<()> {
    let expected = metadata
        .dimensions
        .iter()
        .try_fold(1usize, |total, dimension| {
            total.checked_mul(*dimension as usize)
        })
        .ok_or_else(|| AppError::incompatible("NIfTI dimensions overflow this platform"))?;
    if voxels.len() != expected {
        return Err(AppError::incompatible(format!(
            "voxel payload has {} values; expected {}",
            voxels.len(),
            expected
        )));
    }
    if prefix.len() != metadata.voxel_offset as usize {
        return Err(AppError::incompatible(format!(
            "saved NIfTI prefix has {} bytes; expected {}",
            prefix.len(),
            metadata.voxel_offset
        )));
    }
    let endian = match metadata.endianness.as_str() {
        "little" => Endian::Little,
        "big" => Endian::Big,
        value => {
            return Err(AppError::incompatible(format!(
                "invalid endianness {value}"
            )));
        }
    };
    let file = File::create(path)?;
    if path.to_string_lossy().ends_with(".nii.gz") {
        let encoder = GzEncoder::new(file, Compression::default());
        write_payload(
            BufWriter::new(encoder),
            prefix,
            metadata.datatype,
            endian,
            voxels,
        )
    } else if path.to_string_lossy().ends_with(".nii") {
        write_payload(
            BufWriter::new(file),
            prefix,
            metadata.datatype,
            endian,
            voxels,
        )
    } else {
        Err(AppError::usage("output path must end in .nii or .nii.gz"))
    }
}

fn write_payload<W: Write>(
    mut writer: W,
    prefix: &[u8],
    datatype: u16,
    endian: Endian,
    voxels: &[u32],
) -> Result<()> {
    writer.write_all(prefix)?;
    let mut buffer = Vec::with_capacity(1024 * 1024);
    for label in voxels {
        encode_label(*label, datatype, endian, &mut buffer)?;
        if buffer.len() >= 1024 * 1024 {
            writer.write_all(&buffer)?;
            buffer.clear();
        }
    }
    writer.write_all(&buffer)?;
    writer.flush()?;
    Ok(())
}

fn encode_label(label: u32, datatype: u16, endian: Endian, output: &mut Vec<u8>) -> Result<()> {
    let range_error = || {
        AppError::incompatible(format!(
            "edited label {label} cannot be represented by {}",
            datatype_name(datatype)
        ))
    };
    match datatype {
        2 => output.push(u8::try_from(label).map_err(|_| range_error())?),
        256 => output.push(i8::try_from(label).map_err(|_| range_error())? as u8),
        4 => append_i16(
            output,
            i16::try_from(label).map_err(|_| range_error())?,
            endian,
        ),
        512 => append_u16(
            output,
            u16::try_from(label).map_err(|_| range_error())?,
            endian,
        ),
        8 => append_i32(
            output,
            i32::try_from(label).map_err(|_| range_error())?,
            endian,
        ),
        768 => append_u32(output, label, endian),
        _ => return Err(AppError::incompatible("unsupported output datatype")),
    }
    Ok(())
}

fn parse_label_names(prefix: &[u8], endian: Endian) -> BTreeMap<u32, String> {
    let mut names = BTreeMap::new();
    if prefix.len() <= NIFTI1_MIN_OFFSET || prefix.get(348).copied().unwrap_or(0) == 0 {
        return names;
    }
    let mut position = NIFTI1_MIN_OFFSET;
    while position + 8 <= prefix.len() {
        let size = read_i32(&prefix[position..position + 4], endian);
        if size < 8
            || !(size as usize).is_multiple_of(16)
            || position + size as usize > prefix.len()
        {
            break;
        }
        let payload = &prefix[position + 8..position + size as usize];
        names.extend(parse_xml_labels(payload));
        position += size as usize;
    }
    names.remove(&0);
    names
}

fn parse_xml_labels(payload: &[u8]) -> BTreeMap<u32, String> {
    let payload = payload.strip_suffix(&[0]).unwrap_or(payload);
    let mut reader = XmlReader::from_reader(payload);
    reader.config_mut().trim_text(true);
    let mut names = BTreeMap::new();
    let mut current_label = None;
    loop {
        match reader.read_event() {
            Ok(Event::Start(element)) => {
                if element.name().as_ref().eq_ignore_ascii_case(b"label") {
                    for attribute in element.attributes().flatten() {
                        if [b"key".as_slice(), b"index".as_slice(), b"value".as_slice()]
                            .iter()
                            .any(|name| attribute.key.as_ref().eq_ignore_ascii_case(name))
                        {
                            current_label = std::str::from_utf8(attribute.value.as_ref())
                                .ok()
                                .and_then(|value| value.parse().ok());
                        }
                    }
                }
            }
            Ok(Event::Text(text)) => {
                if let Some(label) = current_label
                    && let Ok(value) = text.decode()
                {
                    let value = value.trim();
                    if !value.is_empty() {
                        names.insert(label, value.to_string());
                    }
                }
            }
            Ok(Event::End(element)) => {
                if element.name().as_ref().eq_ignore_ascii_case(b"label") {
                    current_label = None;
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    names
}

pub fn sha256_file(path: &Path) -> Result<String> {
    let mut file = File::open(path)?;
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 1024 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hash.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hash.finalize()))
}

pub fn sha256_bytes(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

pub fn read_prefix(path: &Path) -> Result<Vec<u8>> {
    fs::read(path).map_err(|error| {
        AppError::new(
            ErrorKind::Io,
            format!("cannot read saved NIfTI prefix {}: {error}", path.display()),
        )
    })
}

fn validate_nifti_path(path: &Path) -> Result<()> {
    let value = path.to_string_lossy();
    if !(value.ends_with(".nii") || value.ends_with(".nii.gz")) {
        return Err(AppError::usage("input path must end in .nii or .nii.gz"));
    }
    Ok(())
}

fn detect_endian(header: &[u8]) -> Result<Endian> {
    let bytes: [u8; 4] = header[0..4].try_into().unwrap();
    if i32::from_le_bytes(bytes) == NIFTI1_HEADER_SIZE as i32 {
        Ok(Endian::Little)
    } else if i32::from_be_bytes(bytes) == NIFTI1_HEADER_SIZE as i32 {
        Ok(Endian::Big)
    } else {
        Err(AppError::incompatible(
            "not a NIfTI-1 header (sizeof_hdr is not 348)",
        ))
    }
}

fn positive_dimension(value: i16, axis: usize) -> Result<u32> {
    u32::try_from(value)
        .ok()
        .filter(|value| *value > 0)
        .ok_or_else(|| AppError::incompatible(format!("invalid dim[{axis}] value {value}")))
}

fn datatype_bits(datatype: u16) -> Result<u16> {
    match datatype {
        2 | 256 => Ok(8),
        4 | 512 => Ok(16),
        8 | 768 => Ok(32),
        _ => Err(AppError::incompatible(format!(
            "{} is not a supported integer label datatype; use uint8, int8, uint16, int16, uint32, or int32",
            datatype_name(datatype)
        ))),
    }
}

pub fn datatype_name(datatype: u16) -> &'static str {
    match datatype {
        2 => "uint8",
        4 => "int16",
        8 => "int32",
        16 => "float32",
        64 => "float64",
        256 => "int8",
        512 => "uint16",
        768 => "uint32",
        _ => "unknown datatype",
    }
}

fn i16_at(bytes: &[u8], offset: usize, endian: Endian) -> i16 {
    read_i16(&bytes[offset..offset + 2], endian)
}

fn f32_at(bytes: &[u8], offset: usize, endian: Endian) -> f32 {
    let raw = read_u32(&bytes[offset..offset + 4], endian);
    f32::from_bits(raw)
}

fn four_f32(bytes: &[u8], offset: usize, endian: Endian) -> [f32; 4] {
    [
        f32_at(bytes, offset, endian),
        f32_at(bytes, offset + 4, endian),
        f32_at(bytes, offset + 8, endian),
        f32_at(bytes, offset + 12, endian),
    ]
}

fn read_i16(bytes: &[u8], endian: Endian) -> i16 {
    let raw: [u8; 2] = bytes.try_into().unwrap();
    match endian {
        Endian::Little => i16::from_le_bytes(raw),
        Endian::Big => i16::from_be_bytes(raw),
    }
}

fn read_u16(bytes: &[u8], endian: Endian) -> u16 {
    let raw: [u8; 2] = bytes.try_into().unwrap();
    match endian {
        Endian::Little => u16::from_le_bytes(raw),
        Endian::Big => u16::from_be_bytes(raw),
    }
}

fn read_i32(bytes: &[u8], endian: Endian) -> i32 {
    let raw: [u8; 4] = bytes.try_into().unwrap();
    match endian {
        Endian::Little => i32::from_le_bytes(raw),
        Endian::Big => i32::from_be_bytes(raw),
    }
}

fn read_u32(bytes: &[u8], endian: Endian) -> u32 {
    let raw: [u8; 4] = bytes.try_into().unwrap();
    match endian {
        Endian::Little => u32::from_le_bytes(raw),
        Endian::Big => u32::from_be_bytes(raw),
    }
}

fn append_i16(output: &mut Vec<u8>, value: i16, endian: Endian) {
    output.extend(match endian {
        Endian::Little => value.to_le_bytes(),
        Endian::Big => value.to_be_bytes(),
    });
}

fn append_u16(output: &mut Vec<u8>, value: u16, endian: Endian) {
    output.extend(match endian {
        Endian::Little => value.to_le_bytes(),
        Endian::Big => value.to_be_bytes(),
    });
}

fn append_i32(output: &mut Vec<u8>, value: i32, endian: Endian) {
    output.extend(match endian {
        Endian::Little => value.to_le_bytes(),
        Endian::Big => value.to_be_bytes(),
    });
}

fn append_u32(output: &mut Vec<u8>, value: u32, endian: Endian) {
    output.extend(match endian {
        Endian::Little => value.to_le_bytes(),
        Endian::Big => value.to_be_bytes(),
    });
}

fn nul_terminated_text(bytes: &[u8]) -> String {
    let end = bytes
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integer_codecs_round_trip() {
        for datatype in [2, 4, 8, 256, 512, 768] {
            for endian in [Endian::Little, Endian::Big] {
                let label = if datatype == 256 { 100 } else { 200 };
                let mut encoded = Vec::new();
                encode_label(label, datatype, endian, &mut encoded).unwrap();
                assert_eq!(decode_label(&encoded, datatype, endian).unwrap(), label);
            }
        }
    }
}
