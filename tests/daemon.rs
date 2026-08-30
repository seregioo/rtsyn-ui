use std::path::PathBuf;

use rtsyn_ui::daemon::{DaemonConfig, DaemonController, DaemonStatus};

#[test]
fn status_is_stopped_without_pid_file_or_api() {
    let controller = DaemonController::new(DaemonConfig {
        api_base_url: "http://127.0.0.1:1".to_string(),
        daemon_bin: PathBuf::from("/tmp/rtsyn-missing-daemon"),
        pid_file: PathBuf::from("/tmp/rtsyn-ui-test-missing.pid"),
    });

    assert_eq!(controller.status(), DaemonStatus::Stopped);
}

#[test]
fn config_keeps_shared_api_base_url() {
    let config = DaemonConfig::new("http://127.0.0.1:17191");

    assert_eq!(config.api_base_url, "http://127.0.0.1:17191");
}

#[test]
fn config_uses_daemon_binary_env_override() {
    std::env::set_var("RTSYN_DAEMON_BIN", "/tmp/rtsyn-test-daemon");

    let config = DaemonConfig::new("http://127.0.0.1:17191");

    assert_eq!(config.daemon_bin, PathBuf::from("/tmp/rtsyn-test-daemon"));

    std::env::remove_var("RTSYN_DAEMON_BIN");
}
