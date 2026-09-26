use crate::error::{AppError, Result};
use crate::manifest::PaletteEntry;
use std::collections::{BTreeMap, HashSet};

pub const MAX_LABELS: usize = 127;

/// A caller-chosen block and/or legend name for one label.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PaletteOverride {
    pub block: Option<String>,
    pub name: Option<String>,
}

pub fn assign_palette(
    counts: &BTreeMap<u32, u64>,
    names: &BTreeMap<u32, String>,
) -> Result<Vec<PaletteEntry>> {
    assign_palette_with(counts, names, &BTreeMap::new())
}

/// Parse a palette file: `{"5": "minecraft:bone_block"}` or
/// `{"5": {"block": "minecraft:bone_block", "name": "skull"}}`.
pub fn parse_palette_overrides(bytes: &[u8]) -> Result<BTreeMap<u32, PaletteOverride>> {
    let value: serde_json::Value = serde_json::from_slice(bytes)
        .map_err(|error| AppError::usage(format!("palette file is not valid JSON: {error}")))?;
    let object = value
        .as_object()
        .ok_or_else(|| AppError::usage("palette file must be a JSON object keyed by label ID"))?;
    let mut overrides = BTreeMap::new();
    for (key, entry) in object {
        let label: u32 = key
            .trim()
            .parse()
            .ok()
            .filter(|label| *label > 0)
            .ok_or_else(|| {
                AppError::usage(format!("palette key {key:?} is not a positive label ID"))
            })?;
        let parsed = match entry {
            serde_json::Value::String(block) => PaletteOverride {
                block: Some(normalize_block(block)),
                name: None,
            },
            serde_json::Value::Object(fields) => {
                let text = |field: &str| -> Result<Option<String>> {
                    match fields.get(field) {
                        None | Some(serde_json::Value::Null) => Ok(None),
                        Some(serde_json::Value::String(value)) if !value.trim().is_empty() => {
                            Ok(Some(value.trim().to_string()))
                        }
                        Some(_) => Err(AppError::usage(format!(
                            "palette label {label}: {field} must be a non-empty string"
                        ))),
                    }
                };
                PaletteOverride {
                    block: text("block")?.map(|block| normalize_block(&block)),
                    name: text("name")?,
                }
            }
            _ => {
                return Err(AppError::usage(format!(
                    "palette label {label} must map to a block ID or an object"
                )));
            }
        };
        overrides.insert(label, parsed);
    }
    Ok(overrides)
}

fn normalize_block(block: &str) -> String {
    let block = block.trim();
    if block.contains(':') {
        block.to_string()
    } else {
        format!("minecraft:{block}")
    }
}

/// Assign blocks with optional caller overrides. Overridden blocks must come
/// from [`block_palette`] and be unique; every other label keeps the automatic
/// assignment, skipping blocks already chosen. Without overrides the result is
/// identical to [`assign_palette`].
pub fn assign_palette_with(
    counts: &BTreeMap<u32, u64>,
    names: &BTreeMap<u32, String>,
    overrides: &BTreeMap<u32, PaletteOverride>,
) -> Result<Vec<PaletteEntry>> {
    let labels: Vec<(u32, u64)> = counts
        .iter()
        .filter(|(label, _)| **label != 0)
        .map(|(label, count)| (*label, *count))
        .collect();
    if labels.len() > MAX_LABELS {
        return Err(AppError::incompatible(format!(
            "label map contains {} nonzero labels; the supported maximum is {}",
            labels.len(),
            MAX_LABELS
        )));
    }

    let single_label = labels.len() == 1;
    let available = block_palette();
    debug_assert_eq!(available.len(), MAX_LABELS);
    let mut used = HashSet::new();
    let allowed: HashSet<&String> = available.iter().collect();
    for (label, entry) in overrides {
        if let Some(block) = &entry.block {
            if !allowed.contains(block) {
                return Err(AppError::usage(format!(
                    "palette label {label}: {block} is not one of the {MAX_LABELS} supported label blocks (see 'nii2mc blocks')"
                )));
            }
            if !used.insert(block.clone()) {
                return Err(AppError::usage(format!(
                    "palette maps more than one label to {block}"
                )));
            }
        }
    }
    let mut result = Vec::with_capacity(labels.len());

    for (index, (label, voxel_count)) in labels.into_iter().enumerate() {
        let chosen = overrides.get(&label);
        let name = chosen
            .and_then(|entry| entry.name.clone())
            .or_else(|| names.get(&label).cloned());
        let block = if let Some(block) = chosen.and_then(|entry| entry.block.clone()) {
            block
        } else {
            let mut candidates: Vec<String> = semantic_candidates(name.as_deref())
                .into_iter()
                .map(str::to_string)
                .collect();
            if single_label && name.is_none() {
                candidates.insert(0, "minecraft:white_concrete".to_string());
            }
            candidates.extend(available.iter().cloned());
            candidates
                .into_iter()
                .find(|candidate| used.insert(candidate.clone()))
                .ok_or_else(|| AppError::incompatible("not enough unique Minecraft blocks"))?
        };
        result.push(PaletteEntry {
            label,
            block,
            name,
            voxel_count,
            legend_position: [0, 0, index as i32],
        });
    }
    Ok(result)
}

fn semantic_candidates(name: Option<&str>) -> Vec<&'static str> {
    let Some(name) = name else {
        return Vec::new();
    };
    let name = name.to_ascii_lowercase();
    if contains_any(
        &name,
        &[
            "bone",
            "vertebra",
            "rib",
            "sternum",
            "sacrum",
            "skull",
            "femur",
            "humerus",
            "clavicula",
            "scapula",
            "hip",
        ],
    ) {
        return vec![
            "minecraft:bone_block",
            "minecraft:calcite",
            "minecraft:quartz_block",
            "minecraft:smooth_quartz",
            "minecraft:light_gray_concrete",
            "minecraft:white_wool",
        ];
    }
    if contains_any(&name, &["artery", "aorta", "heart", "myocard", "blood"]) {
        return vec![
            "minecraft:red_concrete",
            "minecraft:red_wool",
            "minecraft:red_glazed_terracotta",
            "minecraft:red_terracotta",
        ];
    }
    if contains_any(&name, &["vein", "vena_cava", "portal_vein"]) {
        return vec![
            "minecraft:blue_concrete",
            "minecraft:blue_wool",
            "minecraft:blue_glazed_terracotta",
            "minecraft:blue_terracotta",
        ];
    }
    if contains_any(&name, &["lung", "trachea", "bronch"]) {
        return vec![
            "minecraft:light_blue_concrete",
            "minecraft:cyan_concrete",
            "minecraft:light_blue_wool",
            "minecraft:cyan_wool",
        ];
    }
    if contains_any(&name, &["brain", "spinal_cord"]) {
        return vec![
            "minecraft:pink_concrete",
            "minecraft:magenta_concrete",
            "minecraft:pink_wool",
            "minecraft:magenta_wool",
        ];
    }
    if contains_any(&name, &["liver", "spleen", "muscle", "gland", "pancreas"]) {
        return vec![
            "minecraft:brown_concrete",
            "minecraft:orange_concrete",
            "minecraft:purple_concrete",
            "minecraft:brown_wool",
        ];
    }
    if contains_any(
        &name,
        &["kidney", "bladder", "ureter", "prostate", "uterus"],
    ) {
        return vec![
            "minecraft:orange_concrete",
            "minecraft:yellow_concrete",
            "minecraft:orange_wool",
            "minecraft:yellow_wool",
        ];
    }
    Vec::new()
}

fn contains_any(value: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| value.contains(needle))
}

pub fn block_palette() -> Vec<String> {
    let colors = [
        "red",
        "lime",
        "blue",
        "yellow",
        "magenta",
        "cyan",
        "orange",
        "purple",
        "green",
        "light_blue",
        "pink",
        "brown",
        "black",
        "light_gray",
        "gray",
        "white",
    ];
    let mut blocks = Vec::with_capacity(MAX_LABELS);
    for suffix in [
        "concrete",
        "wool",
        "glazed_terracotta",
        "terracotta",
        "stained_glass",
    ] {
        for color in colors {
            blocks.push(format!("minecraft:{color}_{suffix}"));
        }
    }
    blocks.extend(
        [
            "minecraft:bone_block",
            "minecraft:calcite",
            "minecraft:quartz_block",
            "minecraft:chiseled_quartz_block",
            "minecraft:quartz_bricks",
            "minecraft:smooth_quartz",
            "minecraft:iron_block",
            "minecraft:gold_block",
            "minecraft:diamond_block",
            "minecraft:emerald_block",
            "minecraft:lapis_block",
            "minecraft:redstone_block",
            "minecraft:coal_block",
            "minecraft:copper_block",
            "minecraft:amethyst_block",
            "minecraft:prismarine",
            "minecraft:stone",
            "minecraft:cobblestone",
            "minecraft:mossy_cobblestone",
            "minecraft:stone_bricks",
            "minecraft:mossy_stone_bricks",
            "minecraft:cracked_stone_bricks",
            "minecraft:chiseled_stone_bricks",
            "minecraft:granite",
            "minecraft:polished_granite",
            "minecraft:diorite",
            "minecraft:polished_diorite",
            "minecraft:andesite",
            "minecraft:polished_andesite",
            "minecraft:deepslate",
            "minecraft:polished_deepslate",
            "minecraft:bricks",
            "minecraft:oak_planks",
            "minecraft:spruce_planks",
            "minecraft:birch_planks",
            "minecraft:jungle_planks",
            "minecraft:acacia_planks",
            "minecraft:dark_oak_planks",
            "minecraft:mangrove_planks",
            "minecraft:cherry_planks",
            "minecraft:bamboo_planks",
            "minecraft:crimson_planks",
            "minecraft:warped_planks",
            "minecraft:sea_lantern",
            "minecraft:shroomlight",
            "minecraft:glowstone",
            "minecraft:crying_obsidian",
        ]
        .into_iter()
        .map(str::to_string),
    );
    blocks
}

/// Approximate average texture colour of each supported label block, for
/// previews and legends outside Minecraft.
pub fn block_color(block: &str) -> Option<[u8; 3]> {
    let id = block.strip_prefix("minecraft:").unwrap_or(block);
    let dyes: [(&str, [[u8; 3]; 5]); 16] = [
        // concrete, wool, glazed terracotta, terracotta, stained glass
        (
            "white",
            [
                [207, 213, 214],
                [233, 236, 236],
                [188, 212, 202],
                [209, 178, 161],
                [255, 255, 255],
            ],
        ),
        (
            "orange",
            [
                [224, 97, 1],
                [240, 118, 19],
                [154, 147, 91],
                [161, 83, 37],
                [216, 127, 51],
            ],
        ),
        (
            "magenta",
            [
                [169, 48, 159],
                [189, 68, 179],
                [208, 100, 191],
                [149, 88, 108],
                [178, 76, 216],
            ],
        ),
        (
            "light_blue",
            [
                [36, 137, 199],
                [58, 175, 217],
                [94, 164, 208],
                [113, 108, 137],
                [102, 153, 216],
            ],
        ),
        (
            "yellow",
            [
                [241, 175, 21],
                [248, 198, 39],
                [234, 192, 88],
                [186, 133, 35],
                [229, 229, 51],
            ],
        ),
        (
            "lime",
            [
                [94, 169, 24],
                [112, 185, 25],
                [162, 197, 55],
                [103, 117, 52],
                [127, 204, 25],
            ],
        ),
        (
            "pink",
            [
                [214, 101, 143],
                [237, 141, 172],
                [235, 154, 181],
                [161, 78, 78],
                [242, 127, 165],
            ],
        ),
        (
            "gray",
            [
                [55, 58, 62],
                [62, 68, 71],
                [83, 90, 93],
                [57, 42, 35],
                [76, 76, 76],
            ],
        ),
        (
            "light_gray",
            [
                [125, 125, 115],
                [142, 142, 134],
                [144, 166, 167],
                [135, 107, 98],
                [153, 153, 153],
            ],
        ),
        (
            "cyan",
            [
                [21, 119, 136],
                [21, 137, 145],
                [52, 118, 125],
                [87, 91, 91],
                [76, 127, 153],
            ],
        ),
        (
            "purple",
            [
                [100, 32, 156],
                [121, 42, 172],
                [109, 48, 152],
                [118, 70, 86],
                [127, 63, 178],
            ],
        ),
        (
            "blue",
            [
                [45, 47, 143],
                [53, 57, 157],
                [47, 64, 139],
                [74, 59, 91],
                [51, 76, 178],
            ],
        ),
        (
            "brown",
            [
                [96, 60, 32],
                [114, 71, 40],
                [119, 106, 85],
                [77, 51, 36],
                [102, 76, 51],
            ],
        ),
        (
            "green",
            [
                [73, 91, 36],
                [84, 109, 27],
                [117, 142, 67],
                [76, 83, 42],
                [102, 127, 51],
            ],
        ),
        (
            "red",
            [
                [142, 33, 33],
                [161, 39, 34],
                [181, 59, 53],
                [143, 61, 46],
                [153, 51, 51],
            ],
        ),
        (
            "black",
            [
                [8, 10, 15],
                [20, 21, 25],
                [67, 30, 32],
                [37, 22, 16],
                [25, 25, 25],
            ],
        ),
    ];
    let suffixes = [
        "_concrete",
        "_wool",
        "_glazed_terracotta",
        "_terracotta",
        "_stained_glass",
    ];
    for (index, suffix) in suffixes.iter().enumerate() {
        if let Some(color) = id.strip_suffix(suffix) {
            // "light_gray_glazed_terracotta" must not match "_terracotta" first.
            if *suffix == "_terracotta" && color.ends_with("_glazed") {
                continue;
            }
            return dyes
                .iter()
                .find(|(name, _)| *name == color)
                .map(|(_, colors)| colors[index]);
        }
    }
    let rgb = match id {
        "bone_block" => [225, 221, 201],
        "calcite" => [223, 224, 220],
        "quartz_block" => [236, 230, 223],
        "chiseled_quartz_block" => [231, 226, 218],
        "quartz_bricks" => [234, 229, 221],
        "smooth_quartz" => [236, 230, 223],
        "iron_block" => [220, 220, 220],
        "gold_block" => [246, 208, 61],
        "diamond_block" => [98, 237, 228],
        "emerald_block" => [42, 203, 87],
        "lapis_block" => [31, 67, 140],
        "redstone_block" => [175, 24, 5],
        "coal_block" => [16, 16, 16],
        "copper_block" => [192, 107, 79],
        "amethyst_block" => [133, 97, 191],
        "prismarine" => [99, 156, 151],
        "stone" => [125, 125, 125],
        "cobblestone" => [127, 127, 127],
        "mossy_cobblestone" => [110, 118, 94],
        "stone_bricks" => [122, 121, 122],
        "mossy_stone_bricks" => [115, 121, 105],
        "cracked_stone_bricks" => [118, 117, 118],
        "chiseled_stone_bricks" => [119, 118, 119],
        "granite" => [149, 103, 85],
        "polished_granite" => [154, 106, 89],
        "diorite" => [188, 188, 188],
        "polished_diorite" => [192, 193, 194],
        "andesite" => [136, 136, 136],
        "polished_andesite" => [132, 134, 133],
        "deepslate" => [80, 80, 82],
        "polished_deepslate" => [72, 72, 73],
        "bricks" => [150, 97, 83],
        "oak_planks" => [162, 130, 78],
        "spruce_planks" => [114, 84, 48],
        "birch_planks" => [192, 175, 121],
        "jungle_planks" => [160, 115, 80],
        "acacia_planks" => [168, 90, 50],
        "dark_oak_planks" => [66, 43, 20],
        "mangrove_planks" => [117, 54, 48],
        "cherry_planks" => [226, 178, 172],
        "bamboo_planks" => [193, 173, 80],
        "crimson_planks" => [101, 48, 70],
        "warped_planks" => [43, 104, 99],
        "sea_lantern" => [172, 199, 190],
        "shroomlight" => [240, 146, 70],
        "glowstone" => [171, 131, 84],
        "crying_obsidian" => [32, 10, 60],
        _ => return None,
    };
    Some(rgb)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn palette_has_127_unique_blocks() {
        let palette = block_palette();
        assert_eq!(palette.len(), MAX_LABELS);
        assert_eq!(palette.iter().collect::<HashSet<_>>().len(), MAX_LABELS);
    }

    #[test]
    fn every_supported_block_has_a_color() {
        for block in block_palette() {
            assert!(block_color(&block).is_some(), "{block} has no color");
        }
        assert_eq!(
            block_color("minecraft:light_gray_glazed_terracotta"),
            Some([144, 166, 167])
        );
        assert_eq!(
            block_color("minecraft:light_gray_terracotta"),
            Some([135, 107, 98])
        );
    }

    #[test]
    fn overrides_reserve_blocks_and_rename_labels() {
        let counts = BTreeMap::from([(0, 5), (1, 2), (2, 3), (3, 4)]);
        let names = BTreeMap::from([(1, "femur_left".to_string())]);
        let overrides = parse_palette_overrides(
            br#"{"2": "bone_block", "3": {"name": "Heart", "block": "minecraft:red_wool"}}"#,
        )
        .unwrap();
        let palette = assign_palette_with(&counts, &names, &overrides).unwrap();
        assert_eq!(palette[0].block, "minecraft:calcite");
        assert_eq!(palette[1].block, "minecraft:bone_block");
        assert_eq!(palette[2].block, "minecraft:red_wool");
        assert_eq!(palette[2].name.as_deref(), Some("Heart"));
    }

    #[test]
    fn invalid_overrides_are_rejected() {
        let counts = BTreeMap::from([(1, 2), (2, 3)]);
        let names = BTreeMap::new();
        for json in [
            br#"{"1": "minecraft:dirt"}"#.as_slice(),
            br#"{"1": "bone_block", "2": "bone_block"}"#.as_slice(),
        ] {
            let overrides = parse_palette_overrides(json).unwrap();
            assert!(assign_palette_with(&counts, &names, &overrides).is_err());
        }
        assert!(parse_palette_overrides(br#"{"0": "bone_block"}"#).is_err());
        assert!(parse_palette_overrides(br#"[1]"#).is_err());
    }

    #[test]
    fn anatomical_names_choose_sensible_material_families() {
        let counts = BTreeMap::from([(0, 5), (1, 2), (2, 3)]);
        let names = BTreeMap::from([(1, "femur_left".to_string()), (2, "aorta".to_string())]);
        let palette = assign_palette(&counts, &names).unwrap();
        assert_eq!(palette[0].block, "minecraft:bone_block");
        assert_eq!(palette[1].block, "minecraft:red_concrete");
    }
}
