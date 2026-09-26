# Voxel order mirrored anatomy and squashed anisotropic scans

Addressed: 2026-09-26 10:57:46 CEST (+0200) (opt-in; default output unchanged)

Commit before change: `ccaa09eb4365218a36cbee9fa21699205b677974`

## Symptom

Worlds from right-handed voxel grids (for example RAS-canonical NIfTI) appeared mirrored, and scans with thick slices, such as 0.7 × 0.7 × 5 mm CT, looked squashed about seven times along the slice axis.

## Confirmed root cause

The placement maps NIfTI i → X, k → Y, j → Z. Swapping two axes has determinant −1, so a right-handed voxel grid becomes a mirror image in Minecraft. One voxel per block ignores the voxel spacing, so anisotropic spacing distorts proportions. Both follow from the original contract, so the default behaviour is kept.

## Change

`--orient anatomical` reads the sform or qform and builds a grid with +i = Right, +j = Posterior, +k = Superior, which the unchanged placement turns into a proper rotation (+X right, +Y up, +Z posterior). `--block-mm` resamples to cubic blocks with supersampled trilinear coverage voting. Derived grids carry a regenerated header and the original extensions, the manifest records the derivation, and `to-nifti` exports the block grid. Tests check an L-shaped phantom's axes, the roundness and volume of a sphere from 0.5 × 0.5 × 2.5 mm voxels, thin-slab survival, and a scaled round trip.
