# Farming Simulator 25 - Force Feedback Mod (Linux)

A force-feedback enhancer for Farming Simulator 25 on Linux / CachyOS.
FS25's stock FFB is a weak centering spring. This project replaces it with
rich, speed-aware, physics-driven feedback: real centering that grows with
speed, tire slip force, suspension-compression rumble, ground-texture
vibration, slope pull, implement-mass damping, and collision kicks.

Primary target: **Moza R5** (R9/R12/R16/R21 also supported) on CachyOS.
Tested against Logitech G29/G920, Thrustmaster T-series and Fanatec CSL DD
via the generic evdev path.

---

## How it works

FS25 on Linux runs under Proton. The enhancer is two halves working together:

```
  +------------------+    telemetry.bin    +--------------+   /dev/input/eventX
  |  FS25 + Lua mod  |  ---------------->  |  Rust daemon |  -------------->  wheel
  |  (runs in Wine)  |   (shared file)     |   + GUI      |
  +------------------+                     +--------------+
                                                  ^
                                              config.toml
```

1. The **Lua mod** (`FS25_FFBEnhancer`) lives inside FS25's mods folder.
   Every frame it captures steering angle, speed, lateral acceleration,
   per-wheel slip and suspension compression, ground material, pitch/roll,
   engine RPM, attached-implement mass, and collisions, then writes a
   104-byte binary record to a file in the mod settings directory.
2. The **Rust daemon** (`fs25-ffb`) reads the same file (Wine maps it to a
   native Linux path under `~/.local/share/Steam/steamapps/compatdata/...`),
   runs an effect engine, and pushes `FF_CONSTANT` / `FF_SPRING` /
   `FF_DAMPER` / `FF_PERIODIC` effects directly to `/dev/input/eventX`.
3. The **GUI** (same binary, no `--daemon` flag) shares state with the
   daemon over a lock. Sliders take effect live, no restart.

The game's own FFB can stay on - the daemon's effects layer on top.

---

## Quick start (CachyOS / Arch)

```bash
# 1. Clone
git clone https://github.com/RobinNotHood/Farming-Simulator-25---FFB-Mod-Linux.git
cd Farming-Simulator-25---FFB-Mod-Linux

# 2. Install (builds Rust, installs binary, udev rule, systemd user unit,
#    and copies the Lua mod into FS25)
./packaging/install.sh

# 3. Log out and back in so the `input` group takes effect.

# 4. Add Steam launch options for FS25 (Right-click -> Properties):
#       SDL_JOYSTICK_HIDAPI=0 %command%
#    (keeps Moza on the kernel path where FFB works best)

# 5. Launch the GUI from your application menu, or:
fs25-ffb

# 6. Start FS25, enter a vehicle, come back to the GUI's Status tab.
#    You should see live telemetry and the plot animate.
```

### AUR

A `PKGBUILD` is provided in `packaging/`. Build locally:

```bash
cd packaging && makepkg -si
```

---

## CLI

```
fs25-ffb                     # GUI (default)
fs25-ffb --daemon            # headless (used by the systemd unit)
fs25-ffb --diagnose          # run every troubleshooting check, print report
fs25-ffb --test constant     # play 3s of constant force to check the wheel
fs25-ffb --test spring       # play 5s centering spring
fs25-ffb --test damper       # play 5s damper
fs25-ffb --test rumble       # play 3s rumble
fs25-ffb --test sine         # sweep sine-wave period
fs25-ffb --config ./my.toml  # alternate config path
```

---

## Tuning presets (GUI -> Profiles tab)

| Preset    | Feel                                                  |
|-----------|-------------------------------------------------------|
| arcade    | light, twitch-friendly; good for keyboard/wheel swaps |
| realistic | default, tuned for Moza R5 at ~70% base FFB           |
| heavy     | more weight, more damping; great with big implements  |
| quiet     | minimal rumble, low master gain                       |

Every slider on the **Tuning** tab persists immediately to
`~/.config/fs25-ffb/config.toml`.

---

## Hardware notes

### Moza R5 (primary target)

- Vendor `0x346e`, product `0x0002`. Full PIDFF FFB support landed in
  kernel **6.12.24 / 6.13.12 / 6.14.3** and is cleanest on **6.15+** with
  `hid-universal-pidff` mainline.
- Coexists fine with **boxflat**. Boxflat manages static settings (max
  torque, damper, road sensitivity). This daemon writes runtime FFB effects
  via `evdev`, which boxflat does not touch.
- Set base-side FFB strength around 70% in boxflat; use the GUI's
  `master_gain` for fine tune.

### Logitech G29 / G920 / G923

- Vendor `0x046d`, products `c24f` / `c260` / `c261` / `c262`.
- Stock kernel driver or `new-lg4ff` both work. The daemon detects both.

### Thrustmaster T-series, Fanatec CSL/DD, Simucube, Simagic

- All supported via the generic PIDFF path. Fanatec needs kernel **6.6+**
  for mainline `hid-fanatec`.

Run `fs25-ffb --diagnose` for a per-machine readout.

---

## Repository layout

```
.
|-- README.md                  <- you are here
|-- docs/
|   |-- TROUBLESHOOTING.md     <- extensive troubleshooting guide
|   `-- ARCHITECTURE.md        <- deep dive
|-- mod/FS25_FFBEnhancer/      <- the in-game Lua mod
|   |-- modDesc.xml
|   |-- scripts/
|   |   |-- FFBEnhancer.lua
|   |   |-- Telemetry.lua
|   |   |-- IPCWriter.lua
|   |   `-- Utils.lua
|   `-- translations/
|-- daemon/                    <- Rust daemon + GUI (single binary)
|   |-- Cargo.toml
|   `-- src/
|-- packaging/
|   |-- install.sh
|   |-- PKGBUILD
|   |-- 99-fs25-ffb.rules
|   |-- fs25-ffb.service
|   `-- fs25-ffb.desktop
`-- build.sh                   <- one-shot builder
```

---

## Troubleshooting

If something feels off:

1. Open the GUI -> **Troubleshoot** tab (or run `fs25-ffb --diagnose`).
2. Read [docs/TROUBLESHOOTING.md](docs/TROUBLESHOOTING.md) - it covers every
   failure mode we've seen: permissions, kernel version, Moza-specific
   quirks, Proton version, stale telemetry, boxflat interaction, game FFB
   disabled in `game.xml`, etc.

---

## Credits & prior art

- **GIANTS Software** - FS25 Lua scripting interface and the
  [GDN Scripting Book](https://gdn.giants-software.com/lp/scriptingBook.php).
- Forum post
  [*Extended Force Feedback for Wheel*](https://forum.giants-software.com/viewtopic.php?t=209851)
  - independent confirmation that a two-process (C++ on Windows) design
  works. This project uses the same split on Linux with an evdev backend.
- [Linux kernel FFB docs](https://www.kernel.org/doc/html/latest/input/ff.html)
  for the effect ioctl layout.
- [JacKeTUs/universal-pidff](https://github.com/JacKeTUs/universal-pidff)
  for the Moza kernel patches that made FFB work at all.
- [Lawstorant/boxflat](https://github.com/Lawstorant/boxflat) for the Moza
  config companion we coexist with.
- [berarma/oversteer](https://github.com/berarma/oversteer) for inspiration
  on the GUI layout.

## License

MIT.
