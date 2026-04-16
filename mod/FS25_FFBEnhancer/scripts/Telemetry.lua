--
-- FS25 FFB Enhancer - Telemetry.lua
--
-- Extracts force-feedback-relevant physics state from the current vehicle.
--
-- We are deliberately tolerant of missing fields. FS25's Lua object graph
-- changes between patch releases (e.g. Wheels was split into multiple
-- specializations in a 1.3.x update) so we probe for what we need at runtime
-- rather than relying on a fixed shape.
--

Telemetry = {}

local FLAG_IN_VEHICLE   = 0x01
local FLAG_HANDBRAKE    = 0x02
local FLAG_POWER_STEER  = 0x04
local FLAG_REVERSE      = 0x08
local FLAG_AIRBORNE     = 0x10
local FLAG_PTO_ON       = 0x20

-- Cached value smoothing
local lastVel = {0, 0, 0}
local lastT = 0
-- Tracks the last vehicle we dumped diagnostics for (by identity). Lets us
-- print one summary per vehicle change instead of spamming the log.
local lastDumpedVehicle = nil
local collisionDecay = 0

-- Entry-point called by FFBEnhancer every frame. Returns a fresh telemetry
-- table or nil when there is nothing to send (e.g. no controlled vehicle).
function Telemetry.capture(controlledVehicle, dt)
    local t = {}
    t.timestamp = FFBEUtils.now()

    if controlledVehicle == nil then
        -- We still emit a heartbeat so the daemon knows the pipe is live.
        t.flags = 0
        return t
    end

    local flags = FLAG_IN_VEHICLE

    -- Spec_drivable holds most of what used to live directly on the vehicle
    -- in FS22 (rotatedTime, maxRotTime, etc.). FS25 still sometimes mirrors
    -- those onto the vehicle but not on every class, so probe the spec
    -- first and fall back to the bare field.
    local spec_d = FFBEUtils.get(controlledVehicle, "spec_drivable", nil)

    -- ------------------------------------------------------------------
    -- Steering
    -- ------------------------------------------------------------------
    local steering, maxRot, minRot, target
    if spec_d ~= nil and spec_d.rotatedTime ~= nil then
        steering = spec_d.rotatedTime
        maxRot = FFBEUtils.get(spec_d, "maxRotTime", 1)
        minRot = FFBEUtils.get(spec_d, "minRotTime", -1)
        target = FFBEUtils.get(spec_d, "targetRotatedTime", steering)
    else
        steering = FFBEUtils.get(controlledVehicle, "rotatedTime", 0)
        maxRot = FFBEUtils.get(controlledVehicle, "maxRotTime", 1)
        minRot = FFBEUtils.get(controlledVehicle, "minRotTime", -1)
        target = FFBEUtils.get(controlledVehicle, "targetRotatedTime", steering)
    end
    local span = math.max(math.abs(maxRot), math.abs(minRot), 0.0001)
    t.steering_angle = FFBEUtils.clamp(steering / span, -1, 1)
    t.steering_target = FFBEUtils.clamp(target / span, -1, 1)

    -- ------------------------------------------------------------------
    -- Velocity, acceleration, yaw rate
    -- ------------------------------------------------------------------
    local rootNode = FFBEUtils.get(controlledVehicle, "rootNode", 0)
    local vx, vy, vz = 0, 0, 0
    if rootNode ~= 0 then
        local ok, a, b, c = pcall(getLinearVelocity, rootNode)
        if ok then vx, vy, vz = a, b, c end
    end

    -- Forward speed in m/s. Try, in order:
    --   1. vehicle:getLastSpeed()             -> km/h
    --   2. vehicle.lastSpeedReal              -> m/s (FS25 field)
    --   3. sqrt(vx^2 + vz^2)                  -> m/s from world-frame velocity
    --   4. vehicle.lastSpeed * 1000           -> m/s (FS22-style m/ms field)
    local speed = 0
    local okSpeed, kmh = pcall(function() return controlledVehicle:getLastSpeed() end)
    if okSpeed and type(kmh) == "number" and kmh > 0 then
        speed = kmh / 3.6
    else
        local lsr = FFBEUtils.get(controlledVehicle, "lastSpeedReal", nil)
        if type(lsr) == "number" and lsr > 0 then
            speed = lsr
        else
            local worldSpeed = math.sqrt(vx * vx + vz * vz)
            if worldSpeed > 0.05 then
                speed = worldSpeed
            else
                speed = FFBEUtils.get(controlledVehicle, "lastSpeed", 0) * 1000
            end
        end
    end
    t.speed = speed

    if dt > 0 and lastT > 0 then
        t.longitudinal_accel = (speed - lastVel[1]) / dt
        t.lateral_accel = (vx - lastVel[2]) / dt  -- rough; daemon smooths
    else
        t.longitudinal_accel = 0
        t.lateral_accel = 0
    end
    lastVel = {speed, vx, vz}
    lastT = t.timestamp

    local yawRate = 0
    if rootNode ~= 0 then
        local ok, _, wy, _ = pcall(getAngularVelocity, rootNode)
        if ok then yawRate = wy or 0 end
    end
    t.yaw_rate = yawRate

    -- ------------------------------------------------------------------
    -- Pitch / roll
    -- ------------------------------------------------------------------
    if rootNode ~= 0 then
        local ok, dx, dy, dz = pcall(localDirectionToWorld, rootNode, 0, 0, 1)
        if ok then
            -- pitch = asin(dy); sign convention: nose up = positive
            t.pitch = math.asin(FFBEUtils.clamp(dy or 0, -1, 1))
        end
        local ok2, ux, uy, uz = pcall(localDirectionToWorld, rootNode, 1, 0, 0)
        if ok2 then
            t.roll = math.asin(FFBEUtils.clamp(uy or 0, -1, 1))
        end
    end

    -- ------------------------------------------------------------------
    -- Per-wheel data
    -- ------------------------------------------------------------------
    local wheels = FFBEUtils.get(controlledVehicle, "wheels", {})
    local susp = {0, 0, 0, 0}
    local slipF, slipR = 0, 0
    local slipFcount, slipRcount = 0, 0
    local hardness, roughness = 0, 0
    local airborne = true

    for i, wheel in ipairs(wheels) do
        -- Suspension compression: most wheel tables expose
        -- suspTravel and netForce; normalised 0..1 with suspTravel/maxTravel.
        local travel = FFBEUtils.get(wheel, "suspTravel", 0)
        local maxTravel = FFBEUtils.get(wheel, "maxSuspTravel", 0.15)
        local s = FFBEUtils.clamp(travel / math.max(maxTravel, 0.001), 0, 1)
        if i <= 4 then susp[i] = s end

        if FFBEUtils.get(wheel, "hasGroundContact", false) then
            airborne = false
        end

        -- Lateral slip stored as latSlip on most wheel variants. Positive
        -- positionZ means the wheel is in front of the vehicle origin.
        local lat = math.abs(FFBEUtils.get(wheel, "latSlip", 0))
        if FFBEUtils.get(wheel, "positionZ", 0) > 0 then
            slipF = slipF + lat
            slipFcount = slipFcount + 1
        else
            slipR = slipR + lat
            slipRcount = slipRcount + 1
        end

        -- Ground material hash -> hardness/roughness. These heuristics are
        -- refined by the daemon using a material lookup table.
        local gm = FFBEUtils.get(wheel, "contactGroundType", 0)
        hardness = hardness + Telemetry._hardnessOf(gm)
        roughness = roughness + Telemetry._roughnessOf(gm)
    end

    local wn = math.max(#wheels, 1)
    t.susp_fl = susp[1]
    t.susp_fr = susp[2]
    t.susp_rl = susp[3]
    t.susp_rr = susp[4]
    t.slip_front = slipFcount > 0 and slipF / slipFcount or 0
    t.slip_rear  = slipRcount > 0 and slipR / slipRcount or 0
    t.ground_hardness = hardness / wn
    t.ground_roughness = roughness / wn

    if airborne then flags = flags + FLAG_AIRBORNE end

    -- ------------------------------------------------------------------
    -- Engine / drivetrain
    -- ------------------------------------------------------------------
    local motor = FFBEUtils.get(controlledVehicle, "spec_motorized", nil)
    if motor ~= nil and motor.motor ~= nil then
        local rpm = FFBEUtils.get(motor.motor, "lastMotorRpm", 0)
        local maxRpm = FFBEUtils.get(motor.motor, "maxRpm", 2500)
        t.rpm = FFBEUtils.clamp(rpm / math.max(maxRpm, 1), 0, 1.2)
        t.engine_load = FFBEUtils.clamp(
            FFBEUtils.get(motor.motor, "lastMotorAppliedTorque", 0) /
            math.max(FFBEUtils.get(motor.motor, "peakMotorTorque", 1), 1),
            0, 1)
        if FFBEUtils.get(motor, "currentDirection", 1) < 0 then
            flags = flags + FLAG_REVERSE
        end
    end

    local pto = FFBEUtils.get(controlledVehicle, "spec_powerTakeOffs", nil)
    if pto ~= nil and FFBEUtils.get(pto, "activeOutputPtos", 0) > 0 then
        flags = flags + FLAG_PTO_ON
    end

    -- ------------------------------------------------------------------
    -- Mass (self + implements). Prefer the : method call syntax so any
    -- subclass overrides dispatch correctly. FS25 also exposes
    -- spec_attacherJoints.totalMass on some vehicles.
    -- ------------------------------------------------------------------
    local total = 0
    local okT, mt = pcall(function() return controlledVehicle:getTotalMass(true) end)
    if okT and type(mt) == "number" then total = mt end
    t.total_mass = total

    local selfM = 0
    local okS, ms = pcall(function() return controlledVehicle:getTotalMass(false) end)
    if okS and type(ms) == "number" then selfM = ms end
    t.attached_mass = math.max(total - selfM, 0)

    -- ------------------------------------------------------------------
    -- Collision one-shot (decaying)
    -- ------------------------------------------------------------------
    collisionDecay = math.max(0, collisionDecay - dt * 3.0)
    t.collision = collisionDecay

    -- ------------------------------------------------------------------
    -- Flags
    -- ------------------------------------------------------------------
    if FFBEUtils.get(controlledVehicle, "spec_drivable", {}).cruiseControl
       and FFBEUtils.get(controlledVehicle.spec_drivable.cruiseControl, "handbrake", false) then
        flags = flags + FLAG_HANDBRAKE
    end

    -- Power steering: most tractors have it; quads, mowers typically don't.
    -- Use the configuration value when present, otherwise assume true above
    -- 1000 kg.
    local hasPS = FFBEUtils.get(controlledVehicle, "hasPowerSteering",
                                (t.total_mass or 0) > 1000)
    if hasPS then flags = flags + FLAG_POWER_STEER end

    t.flags = flags
    t.vehicle_hash = Telemetry._hashTypeName(
        FFBEUtils.get(controlledVehicle, "typeName", "unknown"))

    if controlledVehicle ~= lastDumpedVehicle then
        lastDumpedVehicle = controlledVehicle
        local okCN, cn = pcall(function() return controlledVehicle.className and controlledVehicle:className() end)
        FFBEUtils.log(
            "vehicle: class=%s type=%s spec_drivable=%s steering=%.3f/%.3f speed=%.2f m/s mass=%.0f kg (self=%.0f) wheels=%d",
            tostring(okCN and cn or "?"),
            tostring(FFBEUtils.get(controlledVehicle, "typeName", "?")),
            tostring(spec_d ~= nil),
            steering or 0, span or 0,
            t.speed or 0,
            t.total_mass or 0,
            selfM or 0,
            #wheels)
    end

    return t
end

-- External call to raise a one-shot collision impulse; FFBEnhancer subscribes
-- to onCollision via the Vehicle spec.
function Telemetry.onCollision(magnitude)
    collisionDecay = math.min(1.0, collisionDecay + FFBEUtils.clamp(magnitude, 0, 1))
end

-- Heuristic mapping from ground type hash to 0..1 hardness.
function Telemetry._hardnessOf(groundType)
    -- The daemon has the authoritative table; we send a rough normalised
    -- score so the wire stays small.
    if groundType == 0 then return 0.5 end
    -- Simple mod-based hash; daemon pairs it with its own lookup.
    local mod = groundType % 7
    if mod == 0 then return 0.9 end  -- asphalt/concrete
    if mod == 1 then return 0.7 end  -- gravel
    if mod == 2 then return 0.4 end  -- dirt
    if mod == 3 then return 0.2 end  -- mud
    if mod == 4 then return 0.3 end  -- ploughed
    if mod == 5 then return 0.6 end  -- grass
    return 0.5
end

function Telemetry._roughnessOf(groundType)
    if groundType == 0 then return 0.0 end
    local mod = groundType % 7
    if mod == 0 then return 0.05 end
    if mod == 1 then return 0.55 end
    if mod == 2 then return 0.35 end
    if mod == 3 then return 0.25 end
    if mod == 4 then return 0.75 end
    if mod == 5 then return 0.20 end
    return 0.2
end

-- djb2-style string hash so the daemon can key per-vehicle profiles.
function Telemetry._hashTypeName(name)
    local h = 5381
    for i = 1, #name do
        h = ((h * 33) + string.byte(name, i)) % 4294967296
    end
    return h
end
