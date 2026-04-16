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

    -- ------------------------------------------------------------------
    -- Steering
    -- ------------------------------------------------------------------
    local steering = FFBEUtils.get(controlledVehicle, "rotatedTime", 0)
    local maxRot = FFBEUtils.get(controlledVehicle, "maxRotTime", 1)
    local minRot = FFBEUtils.get(controlledVehicle, "minRotTime", -1)
    local span = math.max(math.abs(maxRot), math.abs(minRot), 0.0001)
    t.steering_angle = FFBEUtils.clamp(steering / span, -1, 1)

    -- targetRotatedTime is the commanded value from the input binding or AI;
    -- falling back to the actual angle keeps difference-based damping sane.
    local target = FFBEUtils.get(controlledVehicle, "targetRotatedTime", steering)
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

    -- Forward speed (longitudinal) - project world velocity onto vehicle
    -- local Z via the engine helper if available.
    local speed = FFBEUtils.get(controlledVehicle, "lastSpeed", 0) * 1000 / 3600
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

        -- Lateral slip stored as latSlip on most wheel variants.
        local lat = math.abs(FFBEUtils.get(wheel, "latSlip", 0))
        local isFront = FFBEUtils.get(wheel, "isLeft", true)
            and FFBEUtils.get(wheel, "positionZ", 0) > 0
            or FFBEUtils.get(wheel, "positionZ", 0) > 0
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
    -- Mass (self + implements)
    -- ------------------------------------------------------------------
    t.total_mass = FFBEUtils.get(controlledVehicle, "getTotalMass", nil)
    if type(t.total_mass) == "function" then
        local ok, m = pcall(t.total_mass, controlledVehicle, true)
        t.total_mass = ok and m or 0
    end
    t.total_mass = t.total_mass or 0

    local selfMass = FFBEUtils.get(controlledVehicle, "getTotalMass", nil)
    if type(selfMass) == "function" then
        local ok, m = pcall(selfMass, controlledVehicle, false)
        selfMass = ok and m or 0
    end
    t.attached_mass = math.max((t.total_mass or 0) - (selfMass or 0), 0)

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
