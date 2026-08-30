#[cfg(feature = "embedded-daemon")]
use std::ffi::CString;
use std::fs;
use std::fs::OpenOptions;
#[cfg(feature = "embedded-daemon")]
use std::os::raw::{c_char, c_int};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use crate::api::ApiClient;
use crate::{Error, Result, DEFAULT_API_BASE_URL};

pub const RTSYN_DAEMON_BIN_ENV: &str = "RTSYN_DAEMON_BIN";
pub const RTSYN_DAEMON_PID_FILE_ENV: &str = "RTSYN_DAEMON_PID_FILE";
pub const RTSYN_API_HOST_ENV: &str = "RTSYN_API_HOST";
pub const RTSYN_API_PORT_ENV: &str = "RTSYN_API_PORT";

const DEFAULT_PID_FILE: &str = "/tmp/rtsyn-daemon.pid";
const DEFAULT_LOG_FILE: &str = "/tmp/rtsyn-daemon.log";
const START_TIMEOUT: Duration = Duration::from_secs(3);
const STOP_GRACE: Duration = Duration::from_millis(800);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DaemonConfig {
    pub api_base_url: String,
    pub daemon_bin: PathBuf,
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
    daemon_pid: u32,
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
            daemon_bin: env_path_or(RTSYN_DAEMON_BIN_ENV, default_daemon_binary_path()),
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
                    && process_alive(pids.daemon_pid) =>
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

        let mut daemon_command = Command::new(&self.config.daemon_bin);
        configure_daemon_child(&mut daemon_command)?;
        let mut daemon = daemon_command
            .arg("--no-gui")
            .arg("--api")
            .arg(&self.config.api_base_url)
            .arg("daemon")
            .arg("run")
            .spawn()
            .map_err(|error| {
            Error::Api(format!(
                "failed to start RTSyn daemon `{}`: {error}",
                self.config.daemon_bin.display()
            ))
        })?;

        let pids = DaemonPids {
            daemon_pid: daemon.id(),
            api_base_url: Some(self.config.api_base_url.clone()),
        };
        if let Err(error) = write_pid_file(&self.config.pid_file, pids) {
            let _ = terminate_process(daemon.id());
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
            if matches!(daemon.try_wait(), Ok(Some(_))) {
                break;
            }
            thread::sleep(Duration::from_millis(50));
        }

        let _ = terminate_process(daemon.id());
        let _ = fs::remove_file(&self.config.pid_file);
        Err(Error::Api(daemon_start_timeout_error()))
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
        if self.wait_until_status_stopped(STOP_GRACE) {
            self.cleanup_daemon_files();
            return Ok(());
        }

        if let Ok(pids) = read_pid_file(&self.config.pid_file) {
            if pids.api_base_url.as_deref() == Some(self.config.api_base_url.as_str()) {
                terminate_if_alive(pids.daemon_pid);
                if self.wait_until_status_stopped(Duration::from_millis(250)) {
                    self.cleanup_daemon_files();
                    return Ok(());
                }
                kill_if_alive(pids.daemon_pid);
                wait_until_stopped(&[pids.daemon_pid]);
                if self.wait_until_status_stopped(Duration::from_millis(100)) {
                    self.cleanup_daemon_files();
                    return Ok(());
                }
            }
        }

        if let Ok(endpoint) = client.endpoint() {
            for pid in listener_pids_on_port(endpoint.port) {
                terminate_if_alive(pid);
                if self.wait_until_status_stopped(Duration::from_millis(250)) {
                    self.cleanup_daemon_files();
                    return Ok(());
                }
                kill_if_alive(pid);
                wait_until_stopped(&[pid]);
                if self.wait_until_status_stopped(Duration::from_millis(100)) {
                    self.cleanup_daemon_files();
                    return Ok(());
                }
            }
        }

        self.cleanup_daemon_files();
        if self.status() == DaemonStatus::Stopped {
            Ok(())
        } else {
            Err(Error::Api(format!(
                "failed to stop RTSyn daemon at {}",
                self.config.api_base_url
            )))
        }
    }

    fn cleanup_daemon_files(&self) {
        let _ = fs::remove_file(&self.config.pid_file);
        if let Ok(endpoint) = ApiClient::new(&self.config.api_base_url).endpoint() {
            let _ = fs::remove_file(runtime_state_file_for_port(endpoint.port));
        }
    }

    fn wait_until_status_stopped(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if self.status() == DaemonStatus::Stopped {
                return true;
            }
            thread::sleep(Duration::from_millis(25));
        }
        self.status() == DaemonStatus::Stopped
    }
}

#[cfg(feature = "embedded-daemon")]
unsafe extern "C" {
    fn rtsyn_embedded_daemon_run(api_base_url: *const c_char) -> c_int;
}

pub fn run_foreground(api_base_url: &str) -> Result<()> {
    run_embedded_daemon(api_base_url)
}

#[cfg(feature = "embedded-daemon")]
fn run_embedded_daemon(api_base_url: &str) -> Result<()> {
    let api_base_url = CString::new(api_base_url)
        .map_err(|_| Error::Parse("API base URL contains an interior NUL byte".to_string()))?;
    let status = unsafe { rtsyn_embedded_daemon_run(api_base_url.as_ptr()) };
    if status == 0 {
        Ok(())
    } else {
        Err(Error::Api(format!(
            "embedded daemon exited with status {status}"
        )))
    }
}

#[cfg(not(feature = "embedded-daemon"))]
fn run_embedded_daemon(_api_base_url: &str) -> Result<()> {
    Err(Error::Api(
        "embedded daemon backend is not linked in this build".to_string(),
    ))
}

fn configure_daemon_child(command: &mut Command) -> Result<&mut Command> {
    let log = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(DEFAULT_LOG_FILE)?;
    let log_for_stdout = log.try_clone()?;
    command
        .stdin(Stdio::null())
        .stdout(Stdio::from(log_for_stdout))
        .stderr(Stdio::from(log));
    Ok(command)
}

fn daemon_start_timeout_error() -> String {
    let fallback = "daemon did not become ready with compatible API routes".to_string();
    let Ok(contents) = fs::read_to_string(DEFAULT_LOG_FILE) else {
        return fallback;
    };
    let tail = contents
        .lines()
        .rev()
        .take(6)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join("\n");
    if tail.trim().is_empty() {
        fallback
    } else {
        format!("{fallback}\n{tail}")
    }
}

fn env_path_or(name: &str, fallback: PathBuf) -> PathBuf {
    std::env::var_os(name)
        .map(PathBuf::from)
        .unwrap_or(fallback)
}

fn default_daemon_binary_path() -> PathBuf {
    std::env::current_exe().unwrap_or_else(|_| PathBuf::from("rtsyn"))
}

fn write_pid_file(path: &Path, pids: DaemonPids) -> Result<()> {
    fs::write(
        path,
        format!(
            "api_base_url = \"{}\"\ndaemon_pid = {}\n",
            pids.api_base_url.unwrap_or_default(),
            pids.daemon_pid,
        ),
    )?;
    Ok(())
}

fn read_pid_file(path: &Path) -> Result<DaemonPids> {
    let contents = fs::read_to_string(path)?;
    let mut daemon_pid = None;
    let mut api_base_url = None;
    for line in contents.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        match key.trim() {
            "api_base_url" => {
                api_base_url = Some(value.trim().trim_matches('"').to_string());
            }
            "daemon_pid" => {
                daemon_pid = Some(parse_pid(path, value)?);
            }
            _ => {}
        }
    }
    Ok(DaemonPids {
        daemon_pid: daemon_pid
            .ok_or_else(|| Error::Parse(format!("missing daemon pid in `{}`", path.display())))?,
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
    let proc_dir = Path::new("/proc").join(pid.to_string());
    if !proc_dir.exists() {
        return false;
    }
    let stat = fs::read_to_string(proc_dir.join("stat")).unwrap_or_default();
    let state = stat
        .rsplit_once(')')
        .and_then(|(_, rest)| rest.split_whitespace().next());
    !matches!(state, Some("Z"))
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

fn runtime_state_file_for_port(port: u16) -> PathBuf {
    PathBuf::from(format!("/tmp/rtsyn-api-runtime-state-{port}.bin"))
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
