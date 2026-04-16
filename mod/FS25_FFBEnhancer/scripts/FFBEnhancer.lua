--
-- FS25 FFB Enhancer - FFBEnhancer.lua
--
-- Entry point. Wires the Telemetry capture into the mission's update loop
-- and drives IPCWriter at ~60Hz (or whatever the engine tick delivers).
--
-- The mod is fully passive in-game: it does not touch vehicle behaviour,
-- does not register key bindings, and emits no HUD. Everything user-facing
-- lives in the companion Rust GUI.
--

-- Load-time breadcrumb. If this doesn't appear in log.txt, our sourceFile
-- entry in modDesc.xml didn't execute.
print("[FFBEnhancer] source file loaded")

FFBEnhancer = {}
FFBEnhancer.MOD_NAME = g_currentModName or "FS25_FFBEnhancer"
FFBEnhancer.writer = nil
FFBEnhancer.rate = nil
FFBEnhancer.initialized = false

function FFBEnhancer:init()
    if self.initialized then return end
    self.initialized = true

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
    pcall(createFolder, dir)  -- createFolder may not exist in all contexts
    local telemetryPath = FFBEUtils.joinPath(dir, "telemetry.bin")

    self.writer = IPCWriter.new(telemetryPath)
    self.rate = FFBEUtils.makeRateLimiter(120)
    FFBEUtils.log("initialised; writing telemetry to %s", telemetryPath)
end

-- Called every frame. Graceful: if we have no vehicle, still emit a
-- heartbeat so the daemon can distinguish "game running, not driving" from
-- "game not running at all".
function FFBEnhancer:update(dt)
    -- Lazy init on first frame: this avoids any ordering issues between
    -- our sourceFile execution and when the engine is ready for IO.
    if not self.initialized then
        self:init()
    end
    if self.writer == nil then return end
    if self.rate and not self.rate() then return end

    local controlled, source = FFBEnhancer._findControlledVehicle()
    if source ~= self._lastVehicleSource then
        FFBEUtils.log("controlled vehicle source -> %s", tostring(source))
        self._lastVehicleSource = source
    end

    local telemetry = Telemetry.capture(controlled, dt / 1000.0)
    self.writer:write(telemetry)
end

-- Robust vehicle detection for FS25.
--
-- In FS22 the canonical access was `g_currentMission.controlledVehicle`.
-- In FS25 GIANTS refactored the player/vehicle ownership: the bare
-- `.controlledVehicle` field is often nil while the player is actively
-- driving, and the live handle lives on the player entity (or on
-- `g_localPlayer`) behind a `:getCurrentVehicle()` getter. We try every
-- known path in order and return the first non-nil hit, along with a
-- string identifier so the log makes it obvious which one worked.
function FFBEnhancer._findControlledVehicle()
    if g_currentMission == nil then
        return nil, "no g_currentMission"
    end

    local v = g_currentMission.controlledVehicle
    if v ~= nil then return v, "g_currentMission.controlledVehicle" end

    if type(g_currentMission.getCurrentVehicle) == "function" then
        local ok, r = pcall(g_currentMission.getCurrentVehicle, g_currentMission)
        if ok and r ~= nil then return r, "g_currentMission:getCurrentVehicle" end
    end

    local p = g_currentMission.player
    if p ~= nil and type(p.getCurrentVehicle) == "function" then
        local ok, r = pcall(p.getCurrentVehicle, p)
        if ok and r ~= nil then return r, "g_currentMission.player:getCurrentVehicle" end
    end
    if p ~= nil and p.currentVehicle ~= nil then
        return p.currentVehicle, "g_currentMission.player.currentVehicle"
    end

    local lp = _G.g_localPlayer
    if lp ~= nil and type(lp.getCurrentVehicle) == "function" then
        local ok, r = pcall(lp.getCurrentVehicle, lp)
        if ok and r ~= nil then return r, "g_localPlayer:getCurrentVehicle" end
    end
    if lp ~= nil and lp.currentVehicle ~= nil then
        return lp.currentVehicle, "g_localPlayer.currentVehicle"
    end

    return nil, "none"
end

-- ---------------------------------------------------------------------------
-- Hook installation
-- ---------------------------------------------------------------------------
-- FS25 has several mission subclasses (BaseMission, FSBaseMission, Mission00).
-- Due to Lua's non-virtual method lookup, appending to `BaseMission.update`
-- does *not* intercept calls that go via `FSBaseMission:update()` if the
-- subclass has its own update. We therefore append to every class that
-- exists and contains an `update` method; the first one that's actually
-- dispatched wins.

local function onMissionUpdate(_, dt)
    FFBEnhancer:update(dt)
end

local hookCount = 0
local function tryHook(className, classTable)
    if classTable == nil then return end
    if type(classTable.update) ~= "function" then
        print(string.format("[FFBEnhancer] %s.update is not a function (%s)",
            className, type(classTable.update)))
        return
    end
    classTable.update = Utils.appendedFunction(classTable.update, onMissionUpdate)
    hookCount = hookCount + 1
    print(string.format("[FFBEnhancer] hooked %s.update", className))
end

tryHook("BaseMission",   _G.BaseMission)
tryHook("FSBaseMission", _G.FSBaseMission)
tryHook("Mission00",     _G.Mission00)
tryHook("FSCareerMissionInfo", _G.FSCareerMissionInfo)

if hookCount == 0 then
    print("[FFBEnhancer] ERROR: no mission class found at load time. " ..
          "globals present: " ..
          tostring(_G.BaseMission ~= nil) .. "/" ..
          tostring(_G.FSBaseMission ~= nil) .. "/" ..
          tostring(_G.Mission00 ~= nil))
else
    print(string.format("[FFBEnhancer] %d mission hook(s) registered", hookCount))
end

-- Wire collisions into the telemetry one-shot. FS25's high-resolution
-- collider fires onCollision for every ground contact (500-1000 N impulses
-- per rolling wheel), which the old handler turned into a continuous
-- baseline constant force. Filter to real impacts only:
--   * impulse must exceed COLLISION_IMPULSE_MIN (ignore ground noise)
--   * cooldown of COLLISION_COOLDOWN_S between kicks (one-shot, not smear)
local COLLISION_IMPULSE_MIN = 2000   -- N*s
local COLLISION_COOLDOWN_S  = 0.15
local _lastCollisionAt = 0
if Vehicle ~= nil and Vehicle.onCollision ~= nil then
    local oldCollision = Vehicle.onCollision
    Vehicle.onCollision = function(self, transformId1, transformId2, contactNormal, impulse, ...)
        local now = FFBEUtils.now()
        if type(impulse) == "number"
           and impulse >= COLLISION_IMPULSE_MIN
           and (now - _lastCollisionAt) >= COLLISION_COOLDOWN_S then
            _lastCollisionAt = now
            local mag = math.min((impulse - COLLISION_IMPULSE_MIN) / 8000.0, 1.0)
            Telemetry.onCollision(mag)
        end
        return oldCollision(self, transformId1, transformId2, contactNormal, impulse, ...)
    end
    print(string.format(
        "[FFBEnhancer] hooked Vehicle.onCollision (threshold %d N*s, cooldown %.0f ms)",
        COLLISION_IMPULSE_MIN, COLLISION_COOLDOWN_S * 1000))
end
