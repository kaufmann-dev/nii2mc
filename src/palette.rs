use crate::error::{AppError, Result};
use crate::manifest::PaletteEntry;
use std::collections::{BTreeMap, HashSet};

pub const MAX_LABELS: usize = 127;

pub fn assign_palette(
    counts: &BTreeMap<u32, u64>,
    names: &BTreeMap<u32, String>,
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
    let mut result = Vec::with_capacity(labels.len());

    for (index, (label, voxel_count)) in labels.into_iter().enumerate() {
        let name = names.get(&label).cloned();
        let mut candidates: Vec<String> = semantic_candidates(name.as_deref())
            .into_iter()
            .map(str::to_string)
            .collect();
        if single_label && name.is_none() {
            candidates.insert(0, "minecraft:white_concrete".to_string());
        }
        candidates.extend(available.iter().cloned());
        let block = candidates
            .into_iter()
            .find(|candidate| used.insert(candidate.clone()))
            .ok_or_else(|| AppError::incompatible("not enough unique Minecraft blocks"))?;
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
    fn anatomical_names_choose_sensible_material_families() {
        let counts = BTreeMap::from([(0, 5), (1, 2), (2, 3)]);
        let names = BTreeMap::from([(1, "femur_left".to_string()), (2, "aorta".to_string())]);
        let palette = assign_palette(&counts, &names).unwrap();
        assert_eq!(palette[0].block, "minecraft:bone_block");
        assert_eq!(palette[1].block, "minecraft:red_concrete");
    }
}
