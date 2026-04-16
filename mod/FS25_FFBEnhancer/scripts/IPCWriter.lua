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
local FRAME_SIZE = 104

function IPCWriter.new(path)
    local self = setmetatable({}, IPCWriter)
    self.path = path
    self.sequence = 0
    self.file = nil
    self.disabled = false
    self.lastError = nil
    return self
end

-- Try to open the file once. On permission errors we disable ourselves and
-- log; further calls become cheap no-ops.
function IPCWriter:ensureOpen()
    if self.disabled then return false end
    if self.file ~= nil then return true end

    local f, err = io.open(self.path, "wb")
    if f == nil then
        self.lastError = err or "unknown"
        self.disabled = true
        FFBEUtils.log("unable to open telemetry file '%s': %s", self.path, tostring(err))
        return false
    end
    self.file = f
    FFBEUtils.log("telemetry file opened: %s", self.path)
    return true
end

function IPCWriter:close()
    if self.file ~= nil then
        self.file:close()
        self.file = nil
    end
end

-- t is a plain table filled by Telemetry.lua. Missing keys default to 0 so
-- partial telemetry (e.g. when entering a vehicle mid-frame) is still safe.
function IPCWriter:write(t)
    if not self:ensureOpen() then return false end

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

    -- Rewind and overwrite. Keeping the file handle open avoids a syscall
    -- storm at 60Hz; the OS only needs to flush when the block is dirty.
    local ok, err = pcall(function()
        self.file:seek("set", 0)
        self.file:write(table.concat(buf))
        self.file:flush()
    end)

    if not ok then
        self.lastError = err
        self.disabled = true
        FFBEUtils.log("write failed, disabling: %s", tostring(err))
        self:close()
        return false
    end

    return true
end
