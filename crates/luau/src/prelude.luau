-- Runs before any module, while the globals are still writable. It replaces everything whose
-- result depends on memory addresses, hash order, time or randomness.

local rawnext = next
local sort = table.sort
local kindof = type
local raise = error
local count = select
local show = tostring
local format = string.format

-- Numbers sort before strings; anything else can't be a key a module iterates.
local function key_order(a, b)
	local ka, kb = kindof(a), kindof(b)
	if ka ~= kb then
		return ka == "number"
	end
	return a < b
end

-- `pairs` visits keys in sorted order, so iteration never depends on how a table was built.
function pairs(t)
	local keys = {}
	for key in rawnext, t do
		local kind = kindof(key)
		if kind ~= "string" and kind ~= "number" then
			raise("pairs: keys must be strings or numbers, found a " .. kind, 2)
		end
		keys[#keys + 1] = key
	end
	sort(keys, key_order)
	local i = 0
	return function()
		i += 1
		local key = keys[i]
		if key ~= nil then
			return key, t[key]
		end
		return nil
	end
end

local function plain(value)
	local kind = kindof(value)
	return kind == "nil" or kind == "boolean" or kind == "number" or kind == "string"
end

-- The default `tostring` of a table or function includes its address.
function tostring(value)
	if not plain(value) then
		raise("tostring cannot show a " .. kindof(value), 2)
	end
	return show(value)
end

string.format = function(pattern, ...)
	for i = 1, count("#", ...) do
		local value = count(i, ...)
		if not plain(value) then
			raise("string.format cannot show a " .. kindof(value), 2)
		end
	end
	return format(pattern, ...)
end

os = nil
debug = nil
print = nil
collectgarbage = nil
gcinfo = nil
getfenv = nil
setfenv = nil
loadstring = nil
newproxy = nil
require = nil
next = nil
math.random = nil
math.randomseed = nil
table.foreach = nil
table.foreachi = nil
