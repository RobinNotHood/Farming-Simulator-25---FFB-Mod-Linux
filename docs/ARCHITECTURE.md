# Architecture

## Why two processes?

FS25 on Linux runs under Proton (Wine). The Lua sandbox inside FS25 can read
and write files via `io.open()`, but it can't reliably open sockets or call
Linux kernel ioctls. The Linux FFB API is `ioctl()` on `/dev/input/eventX`.
These two worlds meet through the Wine prefix's filesystem, which maps a
Windows path (`C:\Users\steamuser\Documents\My Games\FarmingSimulator2025\`)
to a native Linux directory inside `compatdata`.

So: Lua writes a file, Rust reads it. No sockets, no DLL injection, no Wine
plugin bridges. Just a 104-byte binary record rewritten in place every frame.

## Data flow

```
   (Wine)                              (native Linux)
   FS25 process                        fs25-ffb process
   ---                                 ---
   BaseMission.update                  tokio/std thread
   -> FFBEnhancer:update(dt)            <- std::fs::File::open
      -> Telemetry.capture                 read 104 bytes
         -> IPCWriter:write                parse u32+u16+u16+u32+21*f32+2*u32
            -> io.open("telemetry.bin")    |
               seek(0); write(104B); flush |
                                           v
                                        EffectEngine::compute
                                           |
                                           v
                                        FfbDevice::apply
                                           |
                                           v
                                        ioctl(EVIOCSFF) + write(EV_FF)
                                           |
                                           v
                                        /dev/input/eventX
                                           |
                                           v
                                        steering wheel
```

At 60-120 Hz (Lua write rate) with a 240 Hz daemon tick, we're comfortably
below the latency threshold for felt FFB (~20 ms).

## Why rewrite the whole file?

Torn writes are the main worry. Three reasons we do it anyway:

1. **Size.** 104 bytes is a single block, less than half a page.
2. **Sequence counter.** The reader compares `sequence` to the last seen
   value and discards anything that looks truncated or unchanged.
3. **Stability.** `seek(0) + write + flush` is widely supported across
   Lua versions that ship with FS25; `rename()`-based atomic swaps are
   not.

## Effect pipeline

`EffectEngine::compute(telemetry, tuning, dt)` returns an `EffectOutput`:

| Field              | Meaning                                   |
|--------------------|-------------------------------------------|
| `constant`         | -1..1 lateral pull                        |
| `spring_strength`  | 0..1 centering spring magnitude           |
| `spring_center`    | -1..1 shifted center (slope offset)       |
| `damper`           | 0..1 damper coefficient                   |
| `rumble_magnitude` | 0..1 periodic effect magnitude            |
| `rumble_period_ms` | sine wave period                          |

Each is mapped to an evdev effect:

| EffectOutput         | evdev effect                     |
|----------------------|----------------------------------|
| `constant` + collision | `FF_CONSTANT`                   |
| `spring_*`           | `FF_SPRING`                      |
| `damper`             | `FF_DAMPER`                      |
| `rumble_*`           | `FF_PERIODIC` (sine)             |

All five slots are uploaded once at startup (`EVIOCSFF` with id=-1) and
re-parameterised every tick. Starting/stopping is `write(EV_FF, id, 0|1)`.
Kernel docs: [`ff.rst`](https://www.kernel.org/doc/html/latest/input/ff.html).

## Why egui?

Single Rust binary, no Qt or GTK runtime dependency, identical look across
CachyOS/GNOME/KDE/Sway. The GUI reads `Shared` (a
`Arc<RwLock<LiveState>>`) every frame; the daemon writes to the same lock
at its own rate. Slider edits mutate `Arc<Mutex<Config>>` which the daemon
re-reads each tick - no restart or IPC.

## Why file-based instead of a named pipe?

Named pipes work under Wine (see Wine-Discord-IPC-Bridge), but they add
complexity: a blocked reader dies noisily if the Lua side disappears, and
Wine's pipe-to-unix-socket mapping differs across Proton versions. Files
have no connection state - if the game restarts, the daemon just sees stale
data and fades to zero.

## Thread model

- **Main thread**: eframe GUI in GUI mode, or parked on daemon join in
  daemon mode.
- **Daemon thread**: the tight loop that reads telemetry, computes
  effects, writes FFB. Owns the `FfbDevice`.
- **Test threads**: each "Test" button in the GUI spawns a short-lived
  thread that re-opens the device, plays one effect, and exits. For the
  duration of the test the main daemon thread sees an EBUSY on its
  writes and gracefully degrades.

## Configuration

TOML. Stored at `$XDG_CONFIG_HOME/fs25-ffb/config.toml`. Hand-editing is
supported; the GUI reloads on focus. All fields have defaults so older
configs keep working after upgrades.

## Wire format version

The frame layout carries a `version: u16` and `frame_size: u16`. Version
bumps are rare and always backwards-compatible at the parser level:
future fields are appended after the current tail and old clients ignore
the trailing bytes.
