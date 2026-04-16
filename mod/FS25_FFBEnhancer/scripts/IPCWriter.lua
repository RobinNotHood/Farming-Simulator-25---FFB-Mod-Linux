--
-- FS25 FFB Enhancer - IPCWriter.lua
--
-- Writes a fixed-size binary telemetry frame to a shared file. The Rust
-- daemon (fs25-ffb) watches the same file via inotify and pushes FFB effects
-- to /dev/input/eventX.
--
-- Why a file and not a socket?
--   * Sockets from inside the FS25 Lua sandbox are unreliable across Wine
--     versions. io.open() on a path under getUserProfileAppPath() is stable.
--   * The Wine prefix maps C:\Users\steamuser\Documents\My Games\
--     FarmingSimulator2025\ to a native Linux path inside compatdata, so
--     both halves of the mod see the same inode.
--   * Writes are small (<256 bytes) and atomic enough for our use: we open
--     in "wb" and rewrite the whole file every frame. The reader uses a
--     sequence counter to detect torn reads.
--
-- Binary layout (little-endian):
--   u32  magic            = 0x46464245 ("FFBE")
--   u16  version          = 1
--   u16  frame_size_bytes (for forward compatibility)
--   u32  sequence         (monotonic, increments every write)
--   f32  timestamp_s
--   f32  steering_angle   (-1..1, -1 = full left)
--   f32  steering_target  (-1..1, player / AI intended steering)
--   f32  speed_mps
--   f32  lateral_accel_mps2
--   f32  longitudinal_accel_mps2
--   f32  yaw_rate_rad_s
--   f32  pitch_rad         (+ = nose up)
--   f32  roll_rad          (+ = right side up)
--   f32  tire_slip_front   (0..1, cornering slip)
--   f32  tire_slip_rear
--   f32  susp_compression_fl  (0..1)
--   f32  susp_compression_fr
--   f32  susp_compression_rl
--   f32  susp_compression_rr
--   f32  ground_hardness   (0 = mud .. 1 = asphalt)
--   f32  ground_roughness  (0..1)
--   f32  engine_rpm        (0..1 normalised)
--   f32  engine_load       (0..1)
--   f32  attached_mass_kg
--   f32  total_mass_kg
--   f32  collision_impulse (0..1, decayed one-shot)
--   u32  flags             (bit0=inVehicle, bit1=handbrake, bit2=powerSteering,
--                           bit3=reverse, bit4=airborne, bit5=pto_on)
--   u32  vehicle_type_hash (stable per vehicle class; lets daemon cache)
--
-- Total: 4+2+2+4+4 + 21*4 + 4+4 = 104 bytes
--

IPCWriter = {}
IPCWriter.__index = IPCWriter

local MAGIC = 0x46464245
local VERSION = 1
-- 12-byte header (magic u32 + version u16 + size u16 + sequence u32)
-- + 1 timestamp f32 + 21 physics f32s + 2 trailing u32s (flags + hash)
-- = 12 + 4 + 84 + 8 = 108 bytes.
local FRAME_SIZE = 108

function IPCWriter.new(path)
    local self = setmetatable({}, IPCWriter)
    self.path = path
    self.sequence = 0
    self.disabled = false
    self.lastError = nil
    self.everOpened = false
    return self
end

-- Nothing to hold open; we open-write-close every frame because the GIANTS
-- Lua sandbox doesn't expose `file:seek()`, so we can't rewind a long-lived
-- handle.
function IPCWriter:close() end

-- t is a plain table filled by Telemetry.lua. Missing keys default to 0 so
-- partial telemetry (e.g. when entering a vehicle mid-frame) is still safe.
function IPCWriter:write(t)
    if self.disabled then return false end

    self.sequence = self.sequence + 1

    local buf = {
        FFBEUtils.packU32(MAGIC),
        FFBEUtils.packU16(VERSION),
        FFBEUtils.packU16(FRAME_SIZE),
        FFBEUtils.packU32(self.sequence),
        FFBEUtils.packFloat32(FFBEUtils.get(t, "timestamp", 0)),
        FFBEUtils.packFloat32(FFBEUtils.get(t, "steering_angle", 0)),
        FFBEUtils.packFloat32(FFBEUtils.get(t, "steering_target", 0)),
        FFBEUtils.packFloat32(FFBEUtils.get(t, "speed", 0)),
        FFBEUtils.packFloat32(FFBEUtils.get(t, "lateral_accel", 0)),
        FFBEUtils.packFloat32(FFBEUtils.get(t, "longitudinal_accel", 0)),
        FFBEUtils.packFloat32(FFBEUtils.get(t, "yaw_rate", 0)),
        FFBEUtils.packFloat32(FFBEUtils.get(t, "pitch", 0)),
        FFBEUtils.packFloat32(FFBEUtils.get(t, "roll", 0)),
        FFBEUtils.packFloat32(FFBEUtils.get(t, "slip_front", 0)),
        FFBEUtils.packFloat32(FFBEUtils.get(t, "slip_rear", 0)),
        FFBEUtils.packFloat32(FFBEUtils.get(t, "susp_fl", 0)),
        FFBEUtils.packFloat32(FFBEUtils.get(t, "susp_fr", 0)),
        FFBEUtils.packFloat32(FFBEUtils.get(t, "susp_rl", 0)),
        FFBEUtils.packFloat32(FFBEUtils.get(t, "susp_rr", 0)),
        FFBEUtils.packFloat32(FFBEUtils.get(t, "ground_hardness", 0.5)),
        FFBEUtils.packFloat32(FFBEUtils.get(t, "ground_roughness", 0)),
        FFBEUtils.packFloat32(FFBEUtils.get(t, "rpm", 0)),
        FFBEUtils.packFloat32(FFBEUtils.get(t, "engine_load", 0)),
        FFBEUtils.packFloat32(FFBEUtils.get(t, "attached_mass", 0)),
        FFBEUtils.packFloat32(FFBEUtils.get(t, "total_mass", 0)),
        FFBEUtils.packFloat32(FFBEUtils.get(t, "collision", 0)),
        FFBEUtils.packU32(FFBEUtils.get(t, "flags", 0)),
        FFBEUtils.packU32(FFBEUtils.get(t, "vehicle_hash", 0)),
    }
    local payload = table.concat(buf)

    -- Open with "wb" truncates to zero; write; close. GIANTS' Lua strips
    -- file:seek() so we can't rewind a persistent handle, and the cost of
    -- open/close at <=120Hz is negligible on SSDs.
    local ok, err = pcall(function()
        local f, e = io.open(self.path, "wb")
        if f == nil then error(e or "open failed") end
        f:write(payload)
        f:close()
    end)

    if not ok then
        self.lastError = err
        self.disabled = true
        FFBEUtils.log("write failed, disabling: %s", tostring(err))
        return false
    end

    if not self.everOpened then
        self.everOpened = true
        FFBEUtils.log("first write succeeded (%d bytes) -> %s", #payload, self.path)
    end

    return true
end
