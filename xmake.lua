local project_name = "rtsyn-ui"
local project_xmake_repo = "rtsyn-xmake-repo"

set_license("GPL-3.0-or-later")

add_rules("mode.debug", "mode.release")
set_defaultmode("release")

local cargo = "cargo"
option("thread_core")
set_default("posix")
set_values("posix", "preempt_rt", "xenomai")
set_showmenu(true)
set_description("Thread core backend", "  - posix", "  - preempt_rt", "  - xenomai")
option_end()

local thread_core = get_config("thread_core") or "posix"
local daemon_dependencies = { "rtsyn-engine", "rtsyn-api" }
add_requires("rtsyn-engine", { configs = { thread_core = thread_core } })
add_requires("rtsyn-api")

local workspace = os.getenv("RTSYN_WORKSPACE")
if workspace then
    local repository_dir = path.join(workspace, project_xmake_repo)
    add_repositories(project_xmake_repo .. " " .. repository_dir)
else
    add_repositories(project_xmake_repo .. " https://github.com/seregioo/" .. project_xmake_repo .. ".git")
end

target(project_name)
set_kind("phony")
add_packages(daemon_dependencies)
on_buildcmd(function(_, batchcmds)
    local args = { "build", "--workspace" }
    if is_mode("release") then
        table.insert(args, "--release")
    end
    batchcmds:vrunv(cargo, args)
end)
target("rtsyn-cli")
set_kind("phony")
add_packages(daemon_dependencies)
on_buildcmd(function(_, batchcmds)
    local args = { "build", "--lib" }
    if is_mode("release") then
        table.insert(args, "--release")
    end
    batchcmds:vrunv(cargo, args)
end)

target("rtsyn-gui")
set_kind("phony")
add_packages(daemon_dependencies)
on_buildcmd(function(_, batchcmds)
    local args = { "build", "--manifest-path", "rtsyn-gui/Cargo.toml", "--lib" }
    if is_mode("release") then
        table.insert(args, "--release")
    end
    batchcmds:vrunv(cargo, args)
end)

target("tests/rtsyn-ui-tests")
set_kind("binary")
add_files("xmake/tests_runner.rs")
add_tests("rust-tests")
