# nii2mc

`nii2mc` turns a discrete 3D NIfTI label map into an editable Minecraft Java world and converts the edited blocks back to `.nii` or `.nii.gz`. One NIfTI voxel becomes one Minecraft block. It does not resample the image, so anisotropic medical voxels can look stretched in Minecraft by design.

The generated world targets Minecraft Java Edition 26.2 (world data version 4903). It is a creative, peaceful void world with a lit wireframe around the editable medical volume, a spawn platform, and a block legend outside the export bounds.

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
- If class names are embedded, anatomical keywords guide the palette: bones use pale mineral blocks, arteries use reds, veins use blues, lungs use cyan/light blue, and brain labels use pink/magenta families.
- If names are absent, labels remain unnamed. The tool does not invent anatomical names.

The 127 available label blocks are deterministic, inert, and visually varied. They use concrete, wool, glazed and plain terracotta, stained glass, mineral blocks, stone textures, and planks. Falling blocks, fluids, crops, containers, redstone mechanisms, and other mutation-prone blocks are excluded.

## Coordinate behavior

The selected NIfTI axis maps to Minecraft Y. The other two NIfTI axes, in their original order, map to Minecraft X and Z. No axis is flipped, reoriented, or resampled. X and Z are centered around zero; Y is centered within Minecraft's `-64..319` build range.

A selected vertical axis may contain at most 384 voxels. If it is too long, the error lists the axes that fit.

## Safety and validation

Conversions are staged beside the requested destination and renamed into place only after success. Existing output files and directories are never overwritten.

`validate` and `to-nifti` require an intact `.nii2mc/manifest.json`, the saved NIfTI prefix, and every chunk intersecting the original volume. They refuse to run while Minecraft holds the world's `session.lock`. Unknown blocks report counts and sample coordinates, and export stops before writing any output.

Useful read-only commands are:

```sh
nii2mc inspect labels.nii.gz
nii2mc inspect ./labels-world
nii2mc palette ./labels-world
nii2mc validate ./labels-world
```

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
- Minecraft Java Edition 26.2 worlds created by this tool

NIfTI-2, Analyze pairs, floating-point/probability maps, 4D images, scaled integer payloads, Bedrock Edition, older/newer Minecraft world versions, and arbitrary existing worlds are rejected explicitly.

## Development

```sh
make check
make lint
make test
make build
```

The test suite covers NIfTI integer codecs, semantic palette assignment, Java chunk palette packing, embedded TotalSegmentator-style XML names, negative chunk coordinates, round-trip metadata preservation, stable JSON output, and strict rejection of unknown edited blocks.
