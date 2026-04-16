--
-- FS25 FFB Enhancer - Utils.lua
-- Shared helpers. All functions are intentionally defensive: the GIANTS Lua
-- sandbox evolves between patches, so we wrap every "nice to have" API in
-- pcall() and fall back to a sane default.
--

FFBEUtils = {}

-- Clamp x into [lo, hi]
function FFBEUtils.clamp(x, lo, hi)
    if x ~= x then return 0 end  -- NaN guard
    if x < lo then return lo end
    if x > hi then return hi end
    return x
end

-- Linear remap from [a, b] -> [c, d]
function FFBEUtils.remap(x, a, b, c, d)
    if b == a then return c end
    return c + (x - a) * (d - c) / (b - a)
end

-- Return true if the game exposes getUserProfileAppPath() (it does in FS22/25
-- but not in dedicated server Lua contexts).
function FFBEUtils.getProfilePath()
    local ok, path = pcall(getUserProfileAppPath)
    if ok and type(path) == "string" and #path > 0 then
        return path
    end
    return nil
end

-- Join two path fragments with a forward slash (FS25 Lua uses / even on
-- Windows; Wine translates to the right native separator).
function FFBEUtils.joinPath(a, b)
    if a:sub(-1) == "/" or a:sub(-1) == "\\" then
        return a .. b
    end
    return a .. "/" .. b
end

-- Packs a float into four little-endian bytes. We avoid string.pack because
-- older FS25 Lua releases shipped without the Lua 5.3 pack library.
function FFBEUtils.packFloat32(f)
    if f ~= f then f = 0 end            -- NaN
    if f == math.huge then f = 3.4e38 end
    if f == -math.huge then f = -3.4e38 end

    local sign = 0
    if f < 0 then sign = 1; f = -f end

    local mantissa, exponent
    if f == 0 then
        mantissa = 0
        exponent = 0
    else
        exponent = math.floor(math.log(f) / math.log(2))
        mantissa = f / (2 ^ exponent) - 1
        exponent = exponent + 127
        if exponent < 0 then exponent = 0; mantissa = 0 end
        if exponent > 255 then exponent = 255; mantissa = 0 end
        mantissa = math.floor(mantissa * 2 ^ 23 + 0.5)
    end

    local b0 = mantissa % 256
    local b1 = math.floor(mantissa / 256) % 256
    local b2 = math.floor(mantissa / 65536) % 128 + (exponent % 2) * 128
    local b3 = math.floor(exponent / 2) + sign * 128
    return string.char(b0, b1, b2, b3)
end

-- Packs a uint32 little-endian.
function FFBEUtils.packU32(n)
    n = math.floor(n) % 4294967296
    return string.char(
        n % 256,
        math.floor(n / 256) % 256,
        math.floor(n / 65536) % 256,
        math.floor(n / 16777216) % 256
    )
end

-- Pack a uint16 little-endian.
function FFBEUtils.packU16(n)
    n = math.floor(n) % 65536
    return string.char(n % 256, math.floor(n / 256) % 256)
end

-- Cheap monotonic time in seconds. getTime() is provided by the engine; we
-- fall back to os.clock() during unit tests.
function FFBEUtils.now()
    local ok, t = pcall(getTime)
    if ok and type(t) == "number" then return t end
    return os.clock()
end

-- Safe table access: returns t[k] or default when t is nil / k is absent.
function FFBEUtils.get(t, k, default)
    if t == nil then return default end
    local v = t[k]
    if v == nil then return default end
    return v
end

-- Simple ring-rate limiter. Returns true at most `hz` times per second.
function FFBEUtils.makeRateLimiter(hz)
    local period = 1.0 / hz
    local last = -1
    return function()
        local now = FFBEUtils.now()
        if now - last >= period then
            last = now
            return true
        end
        return false
    end
end

-- Log helper. Prefixes all mod output so it's easy to grep in log.txt.
function FFBEUtils.log(fmt, ...)
    local msg
    if select('#', ...) > 0 then
        msg = string.format(fmt, ...)
    else
        msg = tostring(fmt)
    end
    print("[FFBEnhancer] " .. msg)
end
