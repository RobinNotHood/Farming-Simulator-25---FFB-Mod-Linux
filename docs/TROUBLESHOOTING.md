# FS25 FFB Enhancer - Troubleshooting

This is the long version. For an automated summary run:

```bash
fs25-ffb --diagnose
```

The GUI's **Troubleshoot** tab runs the same checks with a one-click
"copy report" button. Paste the output into an issue if you file one.

---

## Table of contents

1. [Nothing happens / no force feedback at all](#1-nothing-happens)
2. [FFB is still weak after install](#2-ffb-is-still-weak)
3. [Wheel not detected](#3-wheel-not-detected)
4. [Permission denied on /dev/input/eventX](#4-permission-denied)
5. [Moza R5/R9 specific](#5-moza-r5r9-specific)
6. [Logitech G29/G920 specific](#6-logitech-g29g920-specific)
7. [Thrustmaster specific](#7-thrustmaster-specific)
8. [Fanatec specific](#8-fanatec-specific)
9. [Lua mod not loading](#9-lua-mod-not-loading)
10. [Telemetry file is stale or missing](#10-telemetry-stale)
11. [Daemon not starting / crashes](#11-daemon-not-starting)
12. [FFB feels laggy or stuttery](#12-ffb-laggy)
13. [FFB has a constant pull to one side](#13-constant-pull)
14. [Effects fight each other / wheel "pulses"](#14-effects-fight)
15. [CachyOS-specific notes](#15-cachyos-specific)
16. [Steam Deck / Proton notes](#16-steam-deck-proton)
17. [Multiplayer](#17-multiplayer)
18. [Uninstalling](#18-uninstalling)
19. [Reporting a bug](#19-reporting-a-bug)

---

## 1. Nothing happens <a id="1-nothing-happens"></a>

Checklist, in order:

1. **Is the daemon running?**

   ```bash
   systemctl --user status fs25-ffb.service
   # or without systemd:
   pgrep -af fs25-ffb
   ```

   If it's not, start it:

   ```bash
   systemctl --user enable --now fs25-ffb.service
   ```

2. **Does the wheel work at all?** Independent of our mod:

   ```bash
   sudo pacman -S evtest          # (or apt/dnf equivalent)
   sudo evtest                    # pick your wheel, push pedals/turn wheel
   ```

   If evtest shows no events, the wheel is not being seen by the kernel.
   Skip to [§3](#3-wheel-not-detected).

3. **Is FFB actually reaching the kernel?**

   ```bash
   fs25-ffb --test constant
   ```

   This uploads a 40% constant force for three seconds. If the wheel
   physically resists, the daemon path works. If not, see
   [§3](#3-wheel-not-detected) / [§4](#4-permission-denied).

4. **Is FS25 actually writing telemetry?**

   The file lives here on a default Steam install:

   ```
   ~/.local/share/Steam/steamapps/compatdata/2300320/pfx/drive_c/users/steamuser/Documents/My Games/FarmingSimulator2025/modSettings/FS25_FFBEnhancer/telemetry.bin
   ```

   ```bash
   stat "$(fs25-ffb --diagnose 2>/dev/null | grep telemetry | head -1)"
   # or just:
   find ~/.local/share/Steam/steamapps/compatdata/2300320/ -name telemetry.bin 2>/dev/null
   ```

   The modify time should be *now*, not minutes ago, when you're in a
   vehicle. See [§10](#10-telemetry-stale).

5. **Is the game's own FFB enabled?** FS25 ships with FFB on, but some
   users (and some crash recoveries) end up with it off. Check:

   ```
   ~/.local/share/Steam/steamapps/compatdata/2300320/pfx/drive_c/users/steamuser/Documents/My Games/FarmingSimulator2025/game.xml
   ```

   Under `<input>` there should be:

   ```xml
   <forceFeedback enable="true" />
   ```

   (Our daemon works even with FFB disabled in-game, because we write
   directly to evdev. But leaving it on means our effects layer on top of
   the engine's weak ones, which is fine.)

---

## 2. FFB is still weak after install <a id="2-ffb-is-still-weak"></a>

- Open the GUI -> **Tuning** tab. Raise `master_gain` to 1.5 - 2.0.
- Raise `spring_gain` if centering is the issue.
- On Moza: boxflat's "FFB strength" is a *hardware* cap on top of our
  output. If boxflat is at 30% our 100% output ends up as 30%. Set it to
  70% and tune from software.
- If the `constant` and `spring` plot lines on the **Status** tab barely
  move, telemetry is probably not flowing - see [§10](#10-telemetry-stale).
- If they swing but the wheel is calm, the kernel is accepting the ioctl
  but the driver is not producing torque. Run:

  ```bash
  fs25-ffb --test constant
  ```

  If that is also weak, check the vendor utility (boxflat / Oversteer /
  Fanatec Control Panel under Wine) to confirm base-side FFB is at a
  reasonable setting.

---

## 3. Wheel not detected <a id="3-wheel-not-detected"></a>

```bash
lsusb | grep -iE "moza|logitech|thrustmaster|fanatec|simagic|simucube"
```

If nothing shows up, kernel USB/HID enumeration is failing - try:

- Different USB port (preferably USB 2.0 for Moza; some USB 3 hubs cause
  enumeration flapping)
- Different cable
- Power cycle the base
- `dmesg -w` while you plug it in; look for `usb` warnings

If `lsusb` sees it but `/dev/input/eventX` does not:

```bash
ls -la /dev/input/by-id/ | grep -iE "moza|logi|thrustmaster|fanatec"
```

If that is empty, your kernel is missing the driver. Check kernel version:

```bash
uname -r
```

Moza R5/R9 need **6.12.24 / 6.13.12 / 6.14.3 / 6.15+**. On CachyOS:

```bash
sudo pacman -Syu linux-cachyos linux-cachyos-headers
sudo reboot
```

---

## 4. Permission denied on /dev/input/eventX <a id="4-permission-denied"></a>

The daemon needs RW access. Three ways to grant it:

1. **udev rule** (recommended - the installer does this):

   ```bash
   sudo install -m0644 packaging/99-fs25-ffb.rules /etc/udev/rules.d/
   sudo udevadm control --reload
   sudo udevadm trigger
   ```

2. **Add user to `input` group** (fallback):

   ```bash
   sudo gpasswd -a $USER input
   # log out & back in
   ```

3. **Run the daemon as root** (not recommended, but quick):

   ```bash
   sudo fs25-ffb --daemon
   ```

Verify after reload:

```bash
ls -la /dev/input/by-id/ | grep -i moza
# you should see 'rw-rw----' with group 'input' or uaccess ACL
getfacl /dev/input/event12   # replace with your event node
```

---

## 5. Moza R5/R9 specific <a id="5-moza-r5r9-specific"></a>

### Force feedback works but axes don't (or vice versa)

Caused by a SDL/hidraw vs evdev conflict under Proton. In Steam's launch
options for FS25:

```
SDL_JOYSTICK_HIDAPI=0 %command%
```

The daemon's diagnose check flags this.

### Kernel too old

```bash
uname -r
# want: 6.12.24+, 6.13.12+, 6.14.3+, or 6.15+
```

CachyOS:

```bash
sudo pacman -Syu linux-cachyos
sudo reboot
```

If you must stay on an older kernel, install
[`hid-universal-pidff`](https://github.com/JacKeTUs/universal-pidff) as a
DKMS module:

```bash
yay -S universal-pidff-dkms
sudo modprobe -r hid-moza   # if present
sudo modprobe hid-universal-pidff
```

### boxflat not seeing the wheel

boxflat uses hidraw too; SDL_JOYSTICK_HIDAPI does not affect it. Start
boxflat *before* FS25 and leave it running. The daemon uses evdev and
does not conflict.

### FFB direction is backwards

Negative `lateral_gain` in `~/.config/fs25-ffb/config.toml` fixes it, or
in the GUI move `lateral_gain` into the 0..-1 range via the "Reset to
defaults" button and then adjust. (Most Moza bases honour the standard
`direction` field; if yours doesn't, flip the sign.)

### Wheel clicks/pops when the daemon starts

The daemon uploads five effect slots on open. On Moza firmware older than
0.3.7 this can produce an audible pop. Update firmware via Pit House
(Windows VM) or wait - the click is harmless.

---

## 6. Logitech G29/G920 specific <a id="6-logitech-g29g920-specific"></a>

- G29 works with both stock kernel `hid-logitech` and `new-lg4ff`.
- G920 / G923 Xbox edition use HID++ and are **not** supported by
  `new-lg4ff`. Use the mainline kernel (6.3+).
- If you previously used Oversteer's "emulation mode", disable it -
  otherwise the event node you see is the emulated G27 and our daemon
  writes to the wrong device. Point the device hint explicitly:

  Edit `~/.config/fs25-ffb/config.toml`:

  ```toml
  [device_hint]
  name_contains = "G920"
  ```

---

## 7. Thrustmaster specific <a id="7-thrustmaster-specific"></a>

- T150/T248/T300/TX/TS-XW all expose the standard evdev FFB set; no
  firmware tricks needed.
- If the wheel centers too hard at high speed, lower
  `mass_spring_scale` and `spring_gain`.
- TS-XW users: set "dampening" to 0% in the wheel firmware (usually via
  the TM Control Panel under Wine). Our daemon emits its own damper and
  stacking the two produces a mushy feel.

---

## 8. Fanatec specific <a id="8-fanatec-specific"></a>

- Needs kernel **6.6+** for mainline `hid-fanatec`. Older kernels require
  out-of-tree patches.
- CSL DD, DD Pro, DD1, DD2 all work. Set base torque (via tuning menu on
  the wheel) to 50-70%; tune from the GUI.
- Fanatec firmware 450+ adds an internal damper slot. Disable it
  (wheel -> tuning -> DPR = OFF) so ours isn't doubled.

---

## 9. Lua mod not loading <a id="9-lua-mod-not-loading"></a>

Symptoms: no telemetry file appears, no `[FFBEnhancer]` lines in the FS25
log.

1. Confirm the mod is in the right folder:

   ```bash
   ls "$HOME/.local/share/Steam/steamapps/compatdata/2300320/pfx/drive_c/users/steamuser/Documents/My Games/FarmingSimulator2025/mods/" | grep -i FFB
   ```

2. Confirm FS25 sees it. In-game: *Mods -> More info*. If the mod is
   greyed-out, the `modDesc.xml` didn't parse; check:

   ```bash
   tail -200 "$HOME/.local/share/Steam/steamapps/compatdata/2300320/pfx/drive_c/users/steamuser/Documents/My Games/FarmingSimulator2025/log.txt" | grep -iE "ffb|error|warning"
   ```

3. If you installed as a `.zip`, FS25 is picky about zip layout. The
   `modDesc.xml` **must** be at the top of the archive:

   ```
   FS25_FFBEnhancer.zip
     +-- modDesc.xml
     +-- scripts/
     +-- translations/
     `-- icon.dds
   ```

   Not nested under a `FS25_FFBEnhancer/` folder inside the zip. The
   installer builds the zip correctly. If you're doing it by hand:

   ```bash
   cd mod/FS25_FFBEnhancer
   zip -r ../FS25_FFBEnhancer.zip .
   ```

4. In multiplayer, the server must have the mod too. See [§17](#17-multiplayer).

---

## 10. Telemetry file is stale or missing <a id="10-telemetry-stale"></a>

The daemon marks a file "stale" after 500ms with no sequence advance. Causes:

- **FS25 is paused** (menu, ESC). Expected - daemon fades effects to zero
  after 500ms so the wheel doesn't lock up.
- **You're not in a vehicle.** Walking around on foot emits a heartbeat
  but no steering data. Also expected.
- **Mod didn't load.** See [§9](#9-lua-mod-not-loading).
- **Wine path mismatch.** If you moved your Steam library, `getUser
  ProfileAppPath()` may resolve to a compatdata you're not watching. Run
  `fs25-ffb --diagnose` - the `telemetry` check prints the resolved path.
  Override in `~/.config/fs25-ffb/config.toml`:

  ```toml
  [paths]
  telemetry_file = "/mnt/games/steam/steamapps/compatdata/2300320/pfx/drive_c/users/steamuser/Documents/My Games/FarmingSimulator2025/modSettings/FS25_FFBEnhancer/telemetry.bin"
  ```

---

## 11. Daemon not starting / crashes <a id="11-daemon-not-starting"></a>

```bash
journalctl --user -u fs25-ffb -f
# or in a terminal, for verbose logs:
RUST_LOG=debug fs25-ffb --daemon
```

Common errors:

- `no FFB-capable device found` - [§3](#3-wheel-not-detected)
- `EVIOCSFF failed: Operation not permitted` - [§4](#4-permission-denied)
- `EVIOCSFF failed: No space left on device` - the kernel's effect slot
  pool is full. Rare. Reboot the wheel. If it recurs, lower
  `output_hz` in the config (the daemon uploads less often).
- `HOME is not set` - only in exotic systemd setups. Start the daemon
  from your user session instead.

---

## 12. FFB feels laggy or stuttery <a id="12-ffb-laggy"></a>

- Check `output_hz` in config (default 240). Raise to 360-500 on Moza.
  Lower to 120 on slow USB hubs / laptops.
- Check the **Status** tab plot. Latency spikes > 30ms indicate IO
  contention; put the telemetry file on an SSD if you moved your compatdata
  to a spinning disk.
- CPU governor: `powersave` will add jitter. Our diagnose flags this.

  ```bash
  sudo cpupower frequency-set -g performance
  ```

  CachyOS's default `schedutil` is fine.

- MangoHud / gamemoderun: both are fine, no conflict.

---

## 13. FFB has a constant pull to one side <a id="13-constant-pull"></a>

- **Steering calibration in FS25.** Controls -> Wheel -> re-calibrate
  with the wheel physically centered.
- **Autocenter left on in the wheel firmware.** boxflat / TM CP / Fanatec
  tuning menu - set autocenter to **0**. Our daemon turns off autocenter
  on its device, but the wheel may fall back to firmware autocenter if
  the daemon stops.
- **Slope gain too high.** In the Tuning tab, lower `slope_gain` to 0.

---

## 14. Effects fight each other / wheel "pulses" <a id="14-effects-fight"></a>

Usually the Lua mod, the daemon, *and* the wheel's own spring are all
stacking. Disable the wheel's internal spring & damper:

- **Moza (boxflat):** Steering -> `Spring = 0`, `Damper = 0`,
  `Friction = 0`. Leave max torque and FFB strength alone.
- **Logitech G-HUB (under Wine):** centering spring off.
- **Thrustmaster CP:** damping 0, spring 0.

Our daemon emits real spring/damper effects; you want them coming from
exactly one source.

---

## 15. CachyOS-specific notes <a id="15-cachyos-specific"></a>

- **Kernel.** The default `linux-cachyos` is new enough for Moza FFB. If
  you run `linux-lts`, install `universal-pidff-dkms` from AUR or switch.
- **Steam.** CachyOS ships Steam from `multilib` and Flatpak. Both work;
  the Flatpak path is
  `~/.var/app/com.valvesoftware.Steam/.local/share/Steam/...` - the
  daemon autodetects it.
- **Nvidia + Wayland.** If FS25 starts up with black-window issues (not
  FFB-related), switch to X11 for the FS25 session; FFB is unaffected
  either way.
- **SELinux / AppArmor.** CachyOS doesn't ship either by default. If
  you've added them, add an `input` group ACL in your profile.
- **BORE / EEVDF scheduler.** No impact on FFB; noted for completeness.
- **`schedutil` governor.** Fine. Don't switch to `powersave`.

---

## 16. Steam Deck / Proton notes <a id="16-steam-deck-proton"></a>

- Runs in principle. The daemon needs access to `/dev/input/eventX` from
  the desktop session; desktop mode works, Game Mode may not expose it to
  non-Steam binaries.
- Recommend **Proton 9.0+** or **GE-Proton 9-25+**. Older Protons shipped
  a broken dinput joystick stack that silently dropped FFB.
- SteamOS read-only root: install via `flatpak --user` or a nix/home-
  manager config; `./install.sh` will fail to write `/usr/local/bin`.

---

## 17. Multiplayer <a id="17-multiplayer"></a>

- The mod is **client-side only** - it captures what the local player is
  driving and never writes to the save or sends network packets.
- You can still install it on a dedicated server; the Lua side detects
  the absence of `getUserProfileAppPath()` and no-ops.
- On a hosted game, every client who wants enhanced FFB installs it
  themselves. Non-users see no difference.

---

## 18. Uninstalling <a id="18-uninstalling"></a>

```bash
./packaging/install.sh --uninstall
```

This removes:

- `$PREFIX/bin/fs25-ffb`
- `/etc/udev/rules.d/99-fs25-ffb.rules`
- `$HOME/.config/systemd/user/fs25-ffb.service`
- the Lua mod from FS25's mods dir

Config at `~/.config/fs25-ffb/` is preserved. Delete manually if you
want a clean slate.

---

## 19. Reporting a bug <a id="19-reporting-a-bug"></a>

In the GUI's **Troubleshoot** tab, click **Copy report to clipboard**.
Paste that, plus:

- output of `uname -r`, `glxinfo | head -5`
- output of `lsusb | grep -iE "moza|logi|thrust|fana"`
- tail of FS25's `log.txt` around the crash
- tail of `journalctl --user -u fs25-ffb` (or the terminal output if you
  ran `RUST_LOG=debug fs25-ffb --daemon`)

Open an issue at:

<https://github.com/RobinNotHood/Farming-Simulator-25---FFB-Mod-Linux/issues>
