#[test]
fn gui_config_defaults() {
    let config = rtsyn_gui::GuiConfig::default();
    assert_eq!(config.title, "RTSyn");
    assert_eq!(config.width, 1280.0);
    assert_eq!(config.height, 720.0);
}

#[test]
fn daemon_compat_runtime_settings_options_are_local() {
    let response = rtsyn_cli::client::send_request(
        &rtsyn_cli::protocol::DaemonRequest::RuntimeSettingsOptions,
    )
    .expect("compat client should return local settings options");

    match response {
        rtsyn_cli::protocol::DaemonResponse::RuntimeSettingsOptions { options } => {
            assert!(options.frequency_units.iter().any(|unit| unit == "hz"));
            assert!(options.period_units.iter().any(|unit| unit == "ms"));
        }
        other => panic!("unexpected response: {other:?}"),
    }
}

#[test]
fn daemon_compat_plugin_view_subscribes_without_old_transport() {
    let response =
        rtsyn_cli::client::send_request(&rtsyn_cli::protocol::DaemonRequest::RuntimePluginView {
            id: 7,
        })
        .expect("compat client should not use the old daemon transport");

    match response {
        rtsyn_cli::protocol::DaemonResponse::RuntimePluginView { id, .. } => {
            assert_eq!(id, 7);
        }
        other => panic!("unexpected response: {other:?}"),
    }
}
