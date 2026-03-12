-- menu/veld.lua
-- Veld environment status and URL quick-access.

local mod = {}

-- Path to the veld binary.
local VELD_BIN = "/usr/local/bin/veld"

-- ================================
-- DATA
-- ================================

--- Run `veld list --json` and parse the global registry.
local function fetchRegistry()
    local cmd = string.format("%s list --json 2>/dev/null", VELD_BIN)
    local output, status = hs.execute(cmd)

    if not status or not output or output == "" then
        return nil
    end

    local ok, data = pcall(hs.json.decode, output)
    if not ok or not data then
        return nil
    end

    return data
end

--- Filter the registry down to projects that have at least one running run.
local function activeProjects(registry)
    if not registry or not registry.projects then
        return {}
    end

    local result = {}
    for projectKey, project in pairs(registry.projects) do
        local activeRuns = {}
        if project.runs then
            for runName, run in pairs(project.runs) do
                if run.status == "running" or run.status == "starting" then
                    table.insert(activeRuns, {
                        name = runName,
                        status = run.status,
                        urls = run.urls or {},
                    })
                end
            end
        end

        if #activeRuns > 0 then
            -- Sort runs by name for stable ordering.
            table.sort(activeRuns, function(a, b) return a.name < b.name end)
            table.insert(result, {
                key = projectKey,
                name = project.project_name or projectKey,
                root = project.project_root or "",
                runs = activeRuns,
            })
        end
    end

    -- Sort projects by name.
    table.sort(result, function(a, b) return a.name < b.name end)
    return result
end

-- ================================
-- ACTIONS
-- ================================

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

local function stopRun(projectRoot, runName)
    local cmd = string.format(
        "cd %q && %s stop --name %q 2>&1",
        projectRoot, VELD_BIN, runName
    )
    hs.task.new("/bin/sh", function(exitCode, stdOut, stdErr)
        if exitCode == 0 then
            hs.notify.new({
                title = "Veld",
                informativeText = "Stopped: " .. runName,
                withdrawAfter = 3,
            }):send()
        else
            local msg = (stdErr or stdOut or "Unknown error"):sub(1, 200)
            hs.notify.new({
                title = "Veld – Stop Failed",
                informativeText = msg,
                withdrawAfter = 5,
            }):send()
        end
    end, { "-c", cmd }):start()
end

-- ================================
-- MENU BUILDING
-- ================================

local function buildUrlSubmenu(url, nodeLabel)
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

local function buildRunSubmenu(run, project)
    local submenu = {}

    -- Collect and sort URL entries.
    local urlEntries = {}
    for nodeKey, url in pairs(run.urls) do
        table.insert(urlEntries, { node = nodeKey, url = url })
    end
    table.sort(urlEntries, function(a, b) return a.node < b.node end)

    if #urlEntries > 0 then
        table.insert(submenu, { title = "URLs", disabled = true })
        for _, entry in ipairs(urlEntries) do
            table.insert(submenu, {
                title = "  " .. entry.node .. "  " .. entry.url,
                menu = buildUrlSubmenu(entry.url, entry.node),
            })
        end
    else
        table.insert(submenu, { title = "No URLs", disabled = true })
    end

    table.insert(submenu, { title = "-" })

    table.insert(submenu, {
        title = "Stop",
        fn = function()
            local button = hs.dialog.blockAlert(
                "Stop Environment?",
                "Stop run '" .. run.name .. "' in " .. project.name .. "?",
                "Stop",
                "Cancel"
            )
            if button == "Stop" then
                stopRun(project.root, run.name)
            end
        end,
    })

    return submenu
end

-- ================================
-- PUBLIC API
-- ================================

function mod.menuItems()
    local items = {}
    local registry = fetchRegistry()
    local projects = activeProjects(registry)

    table.insert(items, { title = "Veld Environments", disabled = true })

    if #projects == 0 then
        table.insert(items, {
            title = "  No active environments",
            disabled = true,
        })
        return items
    end

    for _, project in ipairs(projects) do
        for _, run in ipairs(project.runs) do
            local statusIcon = run.status == "running" and "●" or "◌"
            local label = string.format("  %s %s / %s", statusIcon, project.name, run.name)

            -- Collect URLs for quick-access at the top level.
            local urlEntries = {}
            for nodeKey, url in pairs(run.urls) do
                table.insert(urlEntries, { node = nodeKey, url = url })
            end
            table.sort(urlEntries, function(a, b) return a.node < b.node end)

            table.insert(items, {
                title = label,
                menu = buildRunSubmenu(run, project),
            })
        end
    end

    return items
end

return mod
