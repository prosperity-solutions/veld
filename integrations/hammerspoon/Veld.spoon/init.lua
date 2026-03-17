--- === Veld ===
---
--- Menu bar integration for Veld, the local development environment orchestrator.
--- Shows active environments with URLs for quick access.
---
--- Install: copy Veld.spoon/ to ~/.hammerspoon/Spoons/
--- Usage:  hs.loadSpoon("Veld"):start()

local obj = {}
obj.__index = obj

-- Metadata
obj.name = "Veld"
obj.version = "1.0"
obj.author = "Prosperity Solutions"
obj.license = "MIT"
obj.homepage = "https://github.com/prosperity-solutions/veld"

-- Configuration

--- Veld.veldBin
--- Variable
--- Path to the veld binary (default: "veld", resolved via the user's login shell PATH).
obj.veldBin = "veld"

-- Internal state
local menubar = nil

-- ============================================================
-- Shell helper
-- ============================================================

--- Run a command through the user's login shell so that PATH from
--- .zshrc / .bashrc / .profile is available.
local function loginShellExecute(cmd)
    local shell = os.getenv("SHELL") or "/bin/zsh"
    return hs.execute(string.format("%s -l -c %q", shell, cmd))
end

-- ============================================================
-- Data
-- ============================================================

--- Fetch all active environments via `veld list --json`.
local function fetchEnvironments(veldBin)
    local output, status = loginShellExecute(
        string.format("%s list --json 2>/dev/null", veldBin)
    )

    if not status or not output or output == "" then
        return {}
    end

    local ok, registry = pcall(hs.json.decode, output)
    if not ok or not registry or not registry.projects then
        return {}
    end

    -- Flatten into a list of active runs with URLs.
    local envs = {}
    for _, project in pairs(registry.projects) do
        if project.runs then
            for runName, run in pairs(project.runs) do
                if run.status == "running" or run.status == "starting" then
                    -- Sort URL entries for stable display.
                    local urls = {}
                    if run.urls then
                        for nodeKey, url in pairs(run.urls) do
                            table.insert(urls, { node = nodeKey, url = url })
                        end
                        table.sort(urls, function(a, b) return a.node < b.node end)
                    end

                    table.insert(envs, {
                        project = project.project_name or "unknown",
                        root = project.project_root or "",
                        run = runName,
                        status = run.status,
                        urls = urls,
                    })
                end
            end
        end
    end

    -- Sort by project then run name.
    table.sort(envs, function(a, b)
        if a.project == b.project then
            return a.run < b.run
        end
        return a.project < b.project
    end)

    return envs
end

-- ============================================================
-- Actions
-- ============================================================

local function openUrl(url)
    hs.urlevent.openURL(url)
end

local function copyUrl(url)
    hs.pasteboard.setContents(url)
    hs.notify.new({
        title = "Veld",
        informativeText = "Copied: " .. url,
        withdrawAfter = 2,
    }):send()
end

local function stopRun(veldBin, projectRoot, runName)
    local cmd = string.format(
        "cd %q && %s stop --name %q 2>&1",
        projectRoot, veldBin, runName
    )
    local shell = os.getenv("SHELL") or "/bin/zsh"
    hs.task.new(shell, function(exitCode, stdOut, stdErr)
        if exitCode == 0 then
            hs.notify.new({
                title = "Veld",
                informativeText = "Stopped: " .. runName,
                withdrawAfter = 3,
            }):send()
        else
            local msg = (stdErr or stdOut or "Unknown error"):sub(1, 200)
            hs.notify.new({
                title = "Veld - Stop Failed",
                informativeText = msg,
                withdrawAfter = 5,
            }):send()
        end
    end, { "-l", "-c", cmd }):start()
end

-- ============================================================
-- Menu building
-- ============================================================

local function buildUrlSubmenu(url)
    return {
        {
            title = "Open in Browser",
            fn = function() openUrl(url) end,
        },
        {
            title = "Copy URL",
            fn = function() copyUrl(url) end,
        },
    }
end

local function buildRunSubmenu(env, veldBin)
    local items = {}

    if #env.urls > 0 then
        for _, entry in ipairs(env.urls) do
            table.insert(items, {
                title = entry.node .. "  " .. entry.url,
                menu = buildUrlSubmenu(entry.url),
            })
        end
    else
        table.insert(items, { title = "No URLs", disabled = true })
    end

    table.insert(items, { title = "-" })

    table.insert(items, {
        title = "Stop",
        fn = function()
            local btn = hs.dialog.blockAlert(
                "Stop Environment?",
                "Stop '" .. env.run .. "' in " .. env.project .. "?",
                "Stop", "Cancel"
            )
            if btn == "Stop" then
                stopRun(veldBin, env.root, env.run)
            end
        end,
    })

    return items
end

local function buildMenu(veldBin)
    local envs = fetchEnvironments(veldBin)
    local items = {}

    if #envs == 0 then
        table.insert(items, { title = "No active environments", disabled = true })
    else
        for _, env in ipairs(envs) do
            local icon = env.status == "running" and "●" or "◌"
            local label = string.format("%s  %s / %s", icon, env.project, env.run)

            table.insert(items, {
                title = label,
                menu = buildRunSubmenu(env, veldBin),
            })
        end
    end

    return items
end

-- ============================================================
-- Spoon lifecycle
-- ============================================================

--- Veld:init()
--- Method
--- Called by hs.loadSpoon(). Sets up state but does not start the menu bar.
function obj:init()
    return self
end

--- Veld:start()
--- Method
--- Create the menu bar item and activate the integration.
function obj:start()
    if menubar then
        menubar:delete()
    end

    menubar = hs.menubar.new(true, "VeldMenuBar")

    -- Use bundled icon if available, fall back to text.
    -- Prefer @2x for Retina clarity; fall back to 1x.
    local spoonPath = hs.spoons.scriptPath()
    local icon = hs.image.imageFromPath(spoonPath .. "/icon@2x.png")
               or hs.image.imageFromPath(spoonPath .. "/icon.png")
    if icon then
        icon:size({ w = 18, h = 18 })
        menubar:setIcon(icon, false)
    else
        menubar:setTitle("V")
    end
    menubar:setTooltip("Veld Environments")

    local bin = self.veldBin
    menubar:setMenu(function()
        return buildMenu(bin)
    end)

    return self
end

--- Veld:stop()
--- Method
--- Remove the menu bar item.
function obj:stop()
    if menubar then
        menubar:delete()
        menubar = nil
    end

    return self
end

return obj
