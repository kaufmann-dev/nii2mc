# Game rules are rejected by Minecraft 26.2

Fixed: 2026-07-22 20:02:20 CEST (+0200)

Commit before fix: `8f6e6f2c14aefdf537527bca28cf8025e176c5ea`

## Symptom

Minecraft Java 26.2 logged a saved-data parse error for `game_rules.dat` when opening a generated world and replaced the requested safety rules with defaults.

## Confirmed root cause

The file used an obsolete `rules` wrapper, legacy camel-case rule names, and string values. Java 26.2 stores game rules directly under `data` with namespaced registry keys and typed byte or integer values.

## Fix

World generation now writes direct Java 26.2 game-rule entries such as `minecraft:advance_time`, `minecraft:spawn_mobs`, and `minecraft:random_tick_speed` with their correct NBT value types. The integration test checks the resulting structure, and Mojang's official Java 26.2 server loads it without a saved-data parse error.
