use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

#[cfg(unix)]
use std::os::unix::process::CommandExt;

use crate::api::ApiClient;
use crate::{Error, Result, DEFAULT_API_BASE_URL};

pub const RTSYN_ENGINE_BIN_ENV: &str = "RTSYN_ENGINE_BIN";
pub const RTSYN_API_BIN_ENV: &str = "RTSYN_API_BIN";
pub const RTSYN_DAEMON_PID_FILE_ENV: &str = "RTSYN_DAEMON_PID_FILE";
pub const RTSYN_API_HOST_ENV: &str = "RTSYN_API_HOST";
pub const RTSYN_API_PORT_ENV: &str = "RTSYN_API_PORT";

const DEFAULT_PID_FILE: &str = "/tmp/rtsyn-daemon.pid";
const START_TIMEOUT: Duration = Duration::from_secs(3);
const STOP_GRACE: Duration = Duration::from_millis(800);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DaemonConfig {
    pub api_base_url: String,
    pub engine_bin: PathBuf,
    pub api_bin: PathBuf,
    pub pid_file: PathBuf,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DaemonStatus {
    Running,
    Stopped,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DaemonController {
    config: DaemonConfig,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct DaemonPids {
    engine_pid: u32,
    api_pid: u32,
    api_base_url: Option<String>,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self::new(DEFAULT_API_BASE_URL)
    }
}

impl DaemonConfig {
    pub fn new(api_base_url: impl Into<String>) -> Self {
        let api_base_url = api_base_url.into();
        Self {
            api_base_url,
            engine_bin: env_path_or(RTSYN_ENGINE_BIN_ENV, default_binary_path("rtsyn-engine")),
            api_bin: env_path_or(RTSYN_API_BIN_ENV, default_binary_path("rtsyn-api")),
            pid_file: env_path_or(RTSYN_DAEMON_PID_FILE_ENV, PathBuf::from(DEFAULT_PID_FILE)),
        }
    }
}

impl DaemonController {
    pub fn new(config: DaemonConfig) -> Self {
        Self { config }
    }

    pub fn default_for_api(api_base_url: impl Into<String>) -> Self {
        Self::new(DaemonConfig::new(api_base_url))
    }

    pub fn config(&self) -> &DaemonConfig {
        &self.config
    }

    pub fn status(&self) -> DaemonStatus {
        let client = ApiClient::new(&self.config.api_base_url);
        if matches!(client.health(), Ok(response) if (200..300).contains(&response.status)) {
            return DaemonStatus::Running;
        }

        match read_pid_file(&self.config.pid_file) {
            Ok(pids)
                if pids.api_base_url.as_deref() == Some(self.config.api_base_url.as_str())
                    && (process_alive(pids.engine_pid) || process_alive(pids.api_pid)) =>
            {
                DaemonStatus::Running
            }
            _ => DaemonStatus::Stopped,
        }
    }

    pub fn is_running(&self) -> bool {
        self.status() == DaemonStatus::Running
    }

    pub fn start(&self) -> Result<String> {
        if self.is_running() {
            if self.node_command_routes_available() {
                return Ok("daemon already running".to_string());
            }
            self.stop_existing_processes()?;
        }

        let endpoint = ApiClient::new(&self.config.api_base_url).endpoint()?;
        let mut engine_command = Command::new(&self.config.engine_bin);
        configure_daemon_child(&mut engine_command);
        let mut engine = engine_command.spawn().map_err(|error| {
            Error::Api(format!(
                "failed to start engine `{}`: {error}",
                self.config.engine_bin.display()
            ))
        })?;

        thread::sleep(Duration::from_millis(100));

        let mut api_command = Command::new(&self.config.api_bin);
        configure_daemon_child(&mut api_command);
        let mut api = api_command
            .env(RTSYN_API_HOST_ENV, endpoint.host)
            .env(RTSYN_API_PORT_ENV, endpoint.port.to_string())
            .spawn()
            .map_err(|error| {
                let _ = terminate_process(engine.id());
                Error::Api(format!(
                    "failed to start API `{}`: {error}",
                    self.config.api_bin.display()
                ))
            })?;

        let pids = DaemonPids {
            engine_pid: engine.id(),
            api_pid: api.id(),
            api_base_url: Some(self.config.api_base_url.clone()),
        };
        if let Err(error) = write_pid_file(&self.config.pid_file, pids) {
            let _ = terminate_process(api.id());
            let _ = terminate_process(engine.id());
            return Err(error);
        }

        let client = ApiClient::new(&self.config.api_base_url);
        let deadline = Instant::now() + START_TIMEOUT;
        while Instant::now() < deadline {
            if matches!(client.health(), Ok(response) if (200..300).contains(&response.status))
                && matches!(client.node_command_routes_available(), Ok(true))
            {
                return Ok(format!("daemon started on {}", self.config.api_base_url));
            }
            if matches!(engine.try_wait(), Ok(Some(_))) || matches!(api.try_wait(), Ok(Some(_))) {
                break;
            }
            thread::sleep(Duration::from_millis(50));
        }

        let _ = terminate_process(api.id());
        let _ = terminate_process(engine.id());
        let _ = fs::remove_file(&self.config.pid_file);
        Err(Error::Api(
            "daemon did not become ready with compatible API routes".to_string(),
        ))
    }

    pub fn stop(&self) -> Result<String> {
        self.stop_existing_processes()?;
        Ok("daemon stopped".to_string())
    }

    fn node_command_routes_available(&self) -> bool {
        matches!(
            ApiClient::new(&self.config.api_base_url).node_command_routes_available(),
            Ok(true)
        )
    }

    fn stop_existing_processes(&self) -> Result<()> {
        let client = ApiClient::new(&self.config.api_base_url);
        let _ = client.stop_engine();

        let mut stopped_any = false;
        if let Ok(pids) = read_pid_file(&self.config.pid_file) {
            if pids.api_base_url.as_deref() == Some(self.config.api_base_url.as_str()) {
                terminate_if_alive(pids.api_pid);
                terminate_if_alive(pids.engine_pid);
                wait_until_stopped(&[pids.api_pid, pids.engine_pid]);
                kill_if_alive(pids.api_pid);
                kill_if_alive(pids.engine_pid);
                stopped_any = true;
            }
        }

        if let Ok(endpoint) = client.endpoint() {
            for pid in listener_pids_on_port(endpoint.port) {
                terminate_if_alive(pid);
                wait_until_stopped(&[pid]);
                kill_if_alive(pid);
                stopped_any = true;
            }
        }

        let _ = fs::remove_file(&self.config.pid_file);
        if stopped_any || !self.is_running() {
            Ok(())
        } else {
            Err(Error::Api(format!(
                "failed to stop RTSyn daemon at {}",
                self.config.api_base_url
            )))
        }
    }
}

fn configure_daemon_child(command: &mut Command) -> &mut Command {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(unix)]
    {
        unsafe {
            command.pre_exec(|| {
                if setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
    command
}

#[cfg(unix)]
unsafe extern "C" {
    fn setsid() -> i32;
}

fn env_path_or(name: &str, fallback: PathBuf) -> PathBuf {
    std::env::var_os(name)
        .map(PathBuf::from)
        .unwrap_or(fallback)
}

fn default_binary_path(name: &str) -> PathBuf {
    for root in binary_search_roots() {
        let local_target = root
            .join("build")
            .join("linux")
            .join("x86_64")
            .join("release")
            .join(name);
        if local_target.is_file() {
            return local_target;
        }

        for candidate in [root.join(name), root.parent().unwrap_or(&root).join(name)]
            .into_iter()
            .map(|module_root| {
                module_root
                    .join("build")
                    .join("linux")
                    .join("x86_64")
                    .join("release")
                    .join(name)
            })
        {
            if candidate.is_file() {
                return candidate;
            }
        }

        if let Some(binary) = find_xmake_package_binary(&root, name) {
            return binary;
        }
    }
    PathBuf::from(name)
}

fn binary_search_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Ok(current_dir) = std::env::current_dir() {
        push_path_and_ancestors(&mut roots, &current_dir);
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            push_path_and_ancestors(&mut roots, parent);
        }
    }
    if let Some(workspace) = std::env::var_os("RTSYN_WORKSPACE") {
        let workspace = PathBuf::from(workspace);
        push_unique_path(&mut roots, workspace.join("rtsyn"));
        push_unique_path(&mut roots, workspace);
    }
    roots
}

fn push_path_and_ancestors(paths: &mut Vec<PathBuf>, path: &Path) {
    for ancestor in path.ancestors().take(8) {
        push_unique_path(paths, ancestor.to_path_buf());
    }
}

fn push_unique_path(paths: &mut Vec<PathBuf>, path: PathBuf) {
    if !paths.iter().any(|existing| existing == &path) {
        paths.push(path);
    }
}

fn find_xmake_package_binary(current_dir: &Path, name: &str) -> Option<PathBuf> {
    let package_root = current_dir
        .join("build")
        .join(".packages")
        .join("r")
        .join(name)
        .join("latest");
    find_binary_under(&package_root, name, 6)
}

fn find_binary_under(root: &Path, name: &str, depth: usize) -> Option<PathBuf> {
    if depth == 0 {
        return None;
    }
    let entries = fs::read_dir(root).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file() && path.file_name().and_then(|file| file.to_str()) == Some(name) {
            return Some(path);
        }
        if path.is_dir() {
            if let Some(binary) = find_binary_under(&path, name, depth - 1) {
                return Some(binary);
            }
        }
    }
    None
}

fn write_pid_file(path: &Path, pids: DaemonPids) -> Result<()> {
    fs::write(
        path,
        format!(
            "api_base_url = \"{}\"\nengine_pid = {}\napi_pid = {}\n",
            pids.api_base_url.unwrap_or_default(),
            pids.engine_pid,
            pids.api_pid
        ),
    )?;
    Ok(())
}

fn read_pid_file(path: &Path) -> Result<DaemonPids> {
    let contents = fs::read_to_string(path)?;
    let mut engine_pid = None;
    let mut api_pid = None;
    let mut api_base_url = None;
    for line in contents.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        match key.trim() {
            "api_base_url" => {
                api_base_url = Some(value.trim().trim_matches('"').to_string());
            }
            "engine_pid" => {
                engine_pid = Some(parse_pid(path, value)?);
            }
            "api_pid" => {
                api_pid = Some(parse_pid(path, value)?);
            }
            _ => {}
        }
    }
    Ok(DaemonPids {
        engine_pid: engine_pid
            .ok_or_else(|| Error::Parse(format!("missing engine pid in `{}`", path.display())))?,
        api_pid: api_pid
            .ok_or_else(|| Error::Parse(format!("missing API pid in `{}`", path.display())))?,
        api_base_url,
    })
}

fn parse_pid(path: &Path, value: &str) -> Result<u32> {
    value
        .trim()
        .parse::<u32>()
        .map_err(|_| Error::Parse(format!("invalid pid file `{}`", path.display())))
}

fn wait_until_stopped(pids: &[u32]) {
    let deadline = Instant::now() + STOP_GRACE;
    while Instant::now() < deadline {
        if pids.iter().all(|pid| !process_alive(*pid)) {
            return;
        }
        thread::sleep(Duration::from_millis(25));
    }
}

fn process_alive(pid: u32) -> bool {
    Path::new("/proc").join(pid.to_string()).exists()
}

fn terminate_if_alive(pid: u32) {
    if process_alive(pid) {
        let _ = terminate_process(pid);
    }
}

fn kill_if_alive(pid: u32) {
    if process_alive(pid) {
        let _ = kill_process(pid);
    }
}

fn terminate_process(pid: u32) -> Result<()> {
    signal_process("-TERM", pid)
}

fn kill_process(pid: u32) -> Result<()> {
    signal_process("-KILL", pid)
}

fn signal_process(signal: &str, pid: u32) -> Result<()> {
    let status = Command::new("kill")
        .arg(signal)
        .arg(pid.to_string())
        .stderr(Stdio::null())
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(Error::Api(format!("failed to send {signal} to pid {pid}")))
    }
}

fn listener_pids_on_port(port: u16) -> Vec<u32> {
    let inodes = listener_socket_inodes(port);
    if inodes.is_empty() {
        return Vec::new();
    }

    let mut pids = Vec::new();
    let Ok(entries) = fs::read_dir("/proc") else {
        return pids;
    };

    for entry in entries.flatten() {
        let file_name = entry.file_name();
        let Some(pid_text) = file_name.to_str() else {
            continue;
        };
        let Ok(pid) = pid_text.parse::<u32>() else {
            continue;
        };
        let fd_dir = entry.path().join("fd");
        let Ok(fds) = fs::read_dir(fd_dir) else {
            continue;
        };
        for fd in fds.flatten() {
            let Ok(target) = fs::read_link(fd.path()) else {
                continue;
            };
            let Some(target) = target.to_str() else {
                continue;
            };
            if let Some(inode) = target
                .strip_prefix("socket:[")
                .and_then(|value| value.strip_suffix(']'))
            {
                if inodes.iter().any(|candidate| candidate == inode) {
                    pids.push(pid);
                    break;
                }
            }
        }
    }
    pids.sort_unstable();
    pids.dedup();
    pids
}

#[cfg(test)]
mod tests {
    use super::find_xmake_package_binary;
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique_temp_dir(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("rtsyn-daemon-{name}-{nanos}"))
    }

    #[test]
    fn finds_binary_in_xmake_package_bin_dir() {
        let root = unique_temp_dir("package-bin");
        let bin_dir = root
            .join("build")
            .join(".packages")
            .join("r")
            .join("rtsyn-engine")
            .join("latest")
            .join("abc")
            .join("bin");
        fs::create_dir_all(&bin_dir).unwrap();
        let binary = bin_dir.join("rtsyn-engine");
        fs::write(&binary, "").unwrap();

        assert_eq!(
            find_xmake_package_binary(&root, "rtsyn-engine"),
            Some(binary)
        );

        fs::remove_dir_all(root).unwrap();
    }
}

fn listener_socket_inodes(port: u16) -> Vec<String> {
    let mut inodes = Vec::new();
    collect_listener_socket_inodes(Path::new("/proc/net/tcp"), port, &mut inodes);
    collect_listener_socket_inodes(Path::new("/proc/net/tcp6"), port, &mut inodes);
    inodes.sort();
    inodes.dedup();
    inodes
}

fn collect_listener_socket_inodes(path: &Path, port: u16, inodes: &mut Vec<String>) {
    let Ok(contents) = fs::read_to_string(path) else {
        return;
    };
    for line in contents.lines().skip(1) {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() <= 9 || fields[3] != "0A" {
            continue;
        }
        let Some(port_hex) = fields[1].rsplit_once(':').map(|(_, port)| port) else {
            continue;
        };
        let Ok(socket_port) = u16::from_str_radix(port_hex, 16) else {
            continue;
        };
        if socket_port == port {
            inodes.push(fields[9].to_string());
        }
    }
}
