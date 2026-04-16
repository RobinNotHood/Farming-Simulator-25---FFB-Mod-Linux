# FS25_FFBEnhancer (Lua mod half)

This is the in-game half of the FS25 FFB Enhancer. It does not modify
gameplay: it only reads vehicle physics each frame and writes them to a
telemetry file for the Linux `fs25-ffb` daemon to consume.

## Files

- `modDesc.xml` - mod descriptor (GIANTS descVersion 103)
- `scripts/FFBEnhancer.lua` - entry point, wires into `BaseMission`
- `scripts/Telemetry.lua` - extracts vehicle physics state
- `scripts/IPCWriter.lua` - binary serializer, writes `telemetry.bin`
- `scripts/Utils.lua` - shared helpers (packing, paths, rate limiting)
- `translations/translation_{en,de}.xml` - localised strings

## Installation

See the repo root `README.md`. Short version:

```bash
./packaging/install.sh --mod-only
```

## Writing the file manually

If the installer can't see your Proton prefix, build and drop the zip
yourself:

```bash
cd mod
zip -r ../dist/FS25_FFBEnhancer.zip FS25_FFBEnhancer \
  -x "*.dds.README"
```

Then copy `dist/FS25_FFBEnhancer.zip` into:

```
~/.local/share/Steam/steamapps/compatdata/2300320/pfx/drive_c/users/steamuser/Documents/My Games/FarmingSimulator2025/mods/
```
