# AGENTS.md

## Purpose and scope

`nii2mc` is a Rust CLI that round-trips discrete 3D NIfTI-1 label maps through Minecraft Java 26.2 worlds. Preserve the default contract: one voxel equals one block, label `0` equals air, no resampling or axis flips, no output overwrite, and no compatibility paths for other Minecraft or NIfTI versions. Resampling (`--block-mm`), anatomical orientation (`--orient anatomical`), and cropping are opt-in only; without them, worlds must stay equivalent to earlier versions (identical manifest, prefix, and chunk contents).

## Repository map

- `src/cli.rs` defines the public commands, JSON envelope, and exit behavior.
- `src/nifti.rs` parses integer NIfTI-1 files and preserves the raw header/extension prefix.
- `src/palette.rs` owns the 127 safe block IDs and semantic label-name preferences.
- `src/world.rs` handles coordinate mapping, manifests, guides, locking, validation, and conversion orchestration.
- `src/resample.rs` owns the opt-in cubic-block resampling, anatomical orientation, and header regeneration for derived grids.
- `src/anvil.rs` writes and reads Java Anvil regions, sections, palettes, heightmaps, and NBT.
- `tests/roundtrip.rs` is the primary cross-format behavior test; `tests/transform.rs` covers resampling, orientation, naming, and palette overrides.

## Required checks

Run the smallest relevant check while developing, then finish with:

```sh
cargo fmt --all
cargo clippy --all-targets -- -D warnings
cargo test --all-targets
cargo build --release
```

Use `make install-local` to install the verified binary to `$HOME/.local/bin`, then run `nii2mc --json doctor` from outside the repository.

## Format invariants

Keep Minecraft Java version `26.2` and data version `4903` synchronized across manifests, chunk NBT, world metadata, CLI output, tests, and README. Use Euclidean division for negative chunk/region coordinates. Block-state indices are ordered X fastest, then Z, then Y, with modern non-spanning long packing and at least four bits per block.

A derived grid (`--block-mm`, `--orient anatomical`) is written with a regenerated header followed by the original extension bytes; the manifest's `transform` records it, and `to-nifti` exports that grid. Keep `transform` absent for default worlds.

Never map an unknown in-volume block to a label. Validation must report it and export must stop before creating output. Guide blocks must remain outside `volume_bounds`. Preserve the original NIfTI prefix byte-for-byte and write edited labels in the original integer datatype and endianness.

Palette additions must be unique, non-falling, non-fluid, inert full cubes without required block entities. Keep `MAX_LABELS` and the palette-length test aligned.

## Documentation

Update `README.md` whenever commands, supported formats, palette behavior, coordinate mapping, safety rules, installation, or Minecraft version changes. Keep examples copy-pasteable and quote paths containing spaces.

