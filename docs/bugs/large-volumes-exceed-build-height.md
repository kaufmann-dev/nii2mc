# Large volumes exceed the build height

Fixed: 2026-07-22 20:02:20 CEST (+0200)

Commit before fix: `8f6e6f2c14aefdf537527bca28cf8025e176c5ea`

## Symptom

`to-world` rejected a NIfTI volume when every axis exceeded 384 voxels, including a 622-voxel vertical axis, with `fitting axes: none`.

## Confirmed root cause

Placement was hard-coded to the standard Overworld range `Y=-64..319`, and the Anvil writer always emitted the corresponding 24 sections and 9-bit heightmaps. The NIfTI data itself was valid.

## Fix

The selected vertical length now determines a Minecraft Java 26.2 dimension height rounded up to a 16-block section boundary. Generated worlds include a namespaced custom Overworld dimension type, the manifest records its bounds, and chunk sections, heightmaps, guides, placement validation, and export all use those bounds. The implementation supports the Java custom-dimension maximum of 4,064 blocks and rejects only larger selected axes.

A 622-voxel regression test verifies a 624-block dimension, both vertical endpoints, generated metadata, validation, and an exact NIfTI round trip. The generated save was also loaded successfully by Mojang's official Java 26.2 server.
