--
-- FS25 FFB Enhancer - FFBEnhancer.lua
--
-- Entry point. Wires the Telemetry capture into the Vehicle.update loop and
-- drives IPCWriter at ~60Hz (or whatever the engine tick delivers).
--
-- The mod is fully passive in-game: it does not touch vehicle behaviour,
-- does not register key bindings, and emits no HUD. Everything user-facing
-- lives in the companion Rust GUI.
--

FFBEnhancer = {}
FFBEnhancer.MOD_NAME = g_currentModName or "FS25_FFBEnhancer"
FFBEnhancer.writer = nil
FFBEnhancer.rate = nil
FFBEnhancer.lastT = 0

function FFBEnhancer:init()
    local profilePath = FFBEUtils.getProfilePath()
    if profilePath == nil then
        FFBEUtils.log("no user profile path - dedicated server? skipping IPC")
        return
    end

    -- modSettings/<mod>/ is the canonical per-mod writable directory in FS25.
    -- We put the telemetry file there so Wine's prefix mapping stays simple:
    --   Windows: C:\Users\steamuser\Documents\My Games\FarmingSimulator2025\
    --            modSettings\FS25_FFBEnhancer\telemetry.bin
    --   Linux:   ~/.local/share/Steam/steamapps/compatdata/2300320/pfx/
    --            drive_c/users/steamuser/Documents/My Games/FarmingSimulator2025/
    --            modSettings/FS25_FFBEnhancer/telemetry.bin
    local dir = FFBEUtils.joinPath(profilePath, "modSettings/FS25_FFBEnhancer")
    createFolder(dir)
    local telemetryPath = FFBEUtils.joinPath(dir, "telemetry.bin")

    self.writer = IPCWriter.new(telemetryPath)
    self.rate = FFBEUtils.makeRateLimiter(120)  -- cap at 120Hz; daemon downsamples
    FFBEUtils.log("initialised; writing telemetry to %s", telemetryPath)
end

-- Called every frame. Graceful: if we have no vehicle, still emit a
-- heartbeat so the daemon can distinguish "game running, not driving" from
-- "game not running at all".
function FFBEnhancer:update(dt)
    if self.writer == nil then return end
    if self.rate and not self.rate() then return end

    local controlled = nil
    local ok, value = pcall(function()
        return g_currentMission and g_currentMission.controlledVehicle or nil
    end)
    if ok then controlled = value end

    local telemetry = Telemetry.capture(controlled, dt / 1000.0)
    self.writer:write(telemetry)
end

function FFBEnhancer:delete()
    if self.writer ~= nil then
        self.writer:close()
        self.writer = nil
    end
end

-- ---------------------------------------------------------------------------
-- Hook installation
-- ---------------------------------------------------------------------------
-- We cannot subclass BaseMission directly from a mod, so we append ourselves
-- to BaseMission.update via Utils.appendedFunction (standard FS25 pattern).

local function onMissionStarted()
    FFBEnhancer:init()
end

local function onMissionUpdate(mission, dt)
    FFBEnhancer:update(dt)
end

local function onMissionDeleted()
    FFBEnhancer:delete()
end

-- The sources run at load time; BaseMission exists by the time modDesc
-- sourceFiles are executed.
if BaseMission ~= nil then
    BaseMission.loadMission = Utils.appendedFunction(
        BaseMission.loadMission, onMissionStarted)
    BaseMission.update = Utils.appendedFunction(
        BaseMission.update, onMissionUpdate)
    BaseMission.delete = Utils.appendedFunction(
        BaseMission.delete, onMissionDeleted)
else
    FFBEUtils.log("WARNING: BaseMission not available at load time")
end

-- Wire collisions into the telemetry one-shot.
if Vehicle ~= nil and Vehicle.onCollision ~= nil then
    local oldCollision = Vehicle.onCollision
    Vehicle.onCollision = function(self, transformId1, transformId2, contactNormal, impulse, ...)
        if impulse ~= nil then
            Telemetry.onCollision(math.min(impulse / 5000.0, 1.0))
        end
        return oldCollision(self, transformId1, transformId2, contactNormal, impulse, ...)
    end
end
