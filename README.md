# nii2mc

`nii2mc` turns a discrete 3D NIfTI label map into an editable Minecraft Java world and converts the edited blocks back to `.nii` or `.nii.gz`. By default one NIfTI voxel becomes one Minecraft block and the image is neither resampled nor reoriented, so anisotropic medical voxels can look stretched. Add `--block-mm` to build cubic blocks of a chosen size and `--orient anatomical` to stand the anatomy upright without mirroring; see [Cubic blocks and anatomical orientation](#cubic-blocks-and-anatomical-orientation).

The generated world targets Minecraft Java Edition 26.2 (world data version 4903). It is a creative, peaceful void world with a lit wireframe around the editable medical volume, a spawn platform, and a block legend outside the export bounds. A bundled data pack gives the Overworld enough vertical height for the selected NIfTI axis.

## Install

You need a current stable Rust toolchain and `make`.

```sh
make install-local
```

This installs `nii2mc` to `$HOME/.local/bin`. Make sure that directory is on `PATH`, then verify the installation from any directory:

```sh
nii2mc --json doctor
```

For development without installing, use `cargo run -- <command>` from this repository.

## Convert, edit, and export

First inspect the input. Quoting paths with spaces is important:

```sh
nii2mc inspect "/path/to/totalsegmentator/segmentations.nii.gz"
```

Create a new world directory:

```sh
nii2mc to-world \
  "/path/to/totalsegmentator/segmentations.nii.gz" \
  --output ./craniofacial-minecraft
```

`z` is the default vertical NIfTI axis. Choose another axis when useful:

```sh
nii2mc to-world labels.nii.gz --output ./labels-world --vertical-axis y
```

Optional world settings:

```sh
nii2mc to-world labels.nii.gz --output ./skull-world \
  --world-name "My skull" \
  --source-name labels.nii.gz \
  --palette palette.json
```

- `--world-name` sets the name shown in Minecraft's world list (default: the output folder name).
- `--source-name` records this file name in the world instead of the real input name, for example to keep a patient name out of the saved world.
- `--palette` chooses blocks and legend names per label. The file is a JSON object such as `{"5": "bone_block", "7": {"block": "minecraft:red_wool", "name": "heart"}}`. Blocks must come from the supported list shown by `nii2mc blocks`, and each block can be used once; labels that are not listed keep the automatic choice.

Place the generated directory in the Minecraft Java saves directory, which is usually `$HOME/.minecraft/saves` on Linux, and open it with Java Edition 26.2. Keep the sea-lantern wireframe: it marks the exact region that will be exported.

Before editing, display the authoritative mapping:

```sh
nii2mc palette ./craniofacial-minecraft
```

Inside the wireframe:

- Air means NIfTI label `0`.
- Breaking a labeled block changes that voxel to `0`.
- Placing a block from the palette changes that voxel to the corresponding label.
- Blocks not listed by `nii2mc palette` are rejected instead of being guessed.
- The frame, platform, and legend are outside the medical volume and are never exported.

Close the world in Minecraft before reading it. Validate the complete volume, then export to a new file:

```sh
nii2mc validate ./craniofacial-minecraft

nii2mc to-nifti ./craniofacial-minecraft \
  --output ./craniofacial-edited.nii.gz
```

The original NIfTI-1 header and extension area are restored byte-for-byte. This preserves the affine, spacing, units, description, datatype, and embedded TotalSegmentator label metadata. Only the voxel payload changes.

## TotalSegmentator inputs

TotalSegmentator normally writes one binary NIfTI file per class. Its `--ml` option instead writes one multilabel NIfTI containing all classes, and current TotalSegmentator files can store class names in the extended header. See the [official TotalSegmentator documentation](https://github.com/wasserth/TotalSegmentator#advanced-settings).

`nii2mc` handles both forms:

- A binary file has one nonzero label and receives one neutral, clearly visible block.
- A multilabel file maps every distinct nonzero integer to a unique block ID.
- Class names are read from the label-table extension whether they are stored as plain text or, as TotalSegmentator writes them, as CDATA.
- If class names are embedded, anatomical keywords guide the palette: bones use pale mineral blocks, arteries use reds, veins use blues, lungs use cyan/light blue, and brain labels use pink/magenta families.
- If names are absent, labels remain unnamed. The tool does not invent anatomical names.

The 127 available label blocks are deterministic, inert, and visually varied. They use concrete, wool, glazed and plain terracotta, stained glass, mineral blocks, stone textures, and planks. Falling blocks, fluids, crops, containers, redstone mechanisms, and other mutation-prone blocks are excluded.

## Cubic blocks and anatomical orientation

Two opt-in options change the grid before the world is built. Without them the output is exactly the one-voxel-per-block world described above.

```sh
nii2mc to-world segmentations.nii.gz --output ./ct-world \
  --block-mm auto --orient anatomical --crop
```

- `--block-mm MM` resamples the label map to cubic blocks of `MM` millimetres. Each block is supersampled; at each sample the surrounding voxels are weighted trilinearly. A block is filled when labels cover at least `--fill-threshold` of it (default `0.5`), and it takes the label covering the most. This keeps volumes and proportions when shrinking, lets one-block-thin structures survive, and avoids terraces when stretching thick slices.
- `--block-mm auto` picks the finest size from 0.25 mm to 20 mm that is not finer than the scan, keeps the tallest axis at or below 320 blocks, and keeps the world at or below 64 million blocks.
- `--orient anatomical` uses the NIfTI sform (or qform) so that superior points up (Minecraft +Y), the patient's right is +X, and posterior is +Z. This is a proper rotation, so the model is not mirrored. Without `--block-mm` it only permutes and flips axes, so every voxel still becomes one block. It always uses Minecraft Y as the vertical axis; do not combine it with another `--vertical-axis`. Files without an sform or qform are rejected.
- `--crop` (with `--block-mm`) limits the grid to the labelled region plus a small margin.

The derived grid gets a regenerated NIfTI header (dimensions, spacing, sform, and qform), while the original extensions, including label names, are kept. The world's manifest records how the grid was derived, and `to-nifti` exports the edited world on this block grid. The byte-for-byte header guarantee applies to worlds made without these options.

To inspect the derived grid without building a world:

```sh
nii2mc resample segmentations.nii.gz --output blocks.nii.gz --block-mm 2 --orient anatomical
```

## Coordinate behavior

The selected NIfTI axis maps to Minecraft Y. The other two NIfTI axes, in their original order, map to Minecraft X and Z. Unless `--orient anatomical` or `--block-mm` is given, no axis is flipped, reoriented, or resampled; because this mapping swaps two axes, an image stored in a right-handed voxel order appears mirrored in Minecraft. X and Z are centered around zero, and Y is centered within the generated dimension.

The dimension keeps Minecraft's standard `-64..319` range for axes up to 384 voxels. For a longer axis, `nii2mc` expands the dimension to the smallest fitting multiple of 16 blocks, up to Minecraft Java's 4,064-block custom-dimension limit. The generated `datapacks/nii2mc` directory is required world data and must remain in place. If the selected axis exceeds 4,064 voxels, the error lists the axes that fit.

## Safety and validation

Conversions are staged beside the requested destination and renamed into place only after success. Existing output files and directories are never overwritten.

`validate` and `to-nifti` require an intact `.nii2mc/manifest.json`, the saved NIfTI prefix, and every chunk intersecting the original volume. They refuse to run while Minecraft holds the world's `session.lock`. Unknown blocks report counts and sample coordinates, and export stops before writing any output.

Useful read-only commands are:

```sh
nii2mc inspect labels.nii.gz
nii2mc inspect ./labels-world
nii2mc palette ./labels-world
nii2mc validate ./labels-world
nii2mc blocks
```

`nii2mc blocks` lists the 127 supported label blocks with an approximate color, useful for legends and previews outside Minecraft.

Add `--json` to any command for a stable stdout envelope:

```json
{
  "ok": true,
  "command": "doctor",
  "data": {}
}
```

Progress is written to stderr. Exit code `2` means invalid arguments or validation input, `3` means incompatible NIfTI/world data, and `4` means an I/O failure.

## Supported input and limits

The first format version intentionally supports only:

- Single-file NIfTI-1 `.nii` and `.nii.gz` images
- Exactly three dimensions
- `uint8`, `int8`, `uint16`, `int16`, `uint32`, or `int32` storage
- Nonnegative discrete labels with identity scaling
- Label `0` as background and at most 127 distinct nonzero labels
- At most 4,064 voxels (or blocks, with `--block-mm`) along the selected vertical axis
- Minecraft Java Edition 26.2 worlds created by this tool

NIfTI-2, Analyze pairs, floating-point/probability maps, 4D images, scaled integer payloads, Bedrock Edition, older/newer Minecraft world versions, and arbitrary existing worlds are rejected explicitly.

## Development

```sh
make check
make lint
make test
make build
```

The test suite covers NIfTI integer codecs, semantic palette assignment and overrides, Java chunk palette packing, embedded TotalSegmentator-style XML names (plain text and CDATA), negative chunk coordinates, round-trip metadata preservation, cubic-block resampling (volume, thin structures, round spheres from anisotropic voxels), anatomical orientation without mirroring, scaled round-trips, stable JSON output, and strict rejection of unknown edited blocks.
