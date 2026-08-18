use crate::protocol::{
    ConnectionSummary, DaemonRequest, DaemonResponse, PluginRequest, RuntimePluginState,
    RuntimeSettingsOptions, DEFAULT_SOCKET_PATH,
};
use rtsyn_ui::api::{ApiClient, NodeKind, NodeState, ValueType};
use rtsyn_ui::daemon::{DaemonConfig, DaemonController};

pub fn send_request(request: &DaemonRequest) -> Result<DaemonResponse, String> {
    send_request_to(DEFAULT_SOCKET_PATH, request)
}

pub fn send_request_to(_path: &str, request: &DaemonRequest) -> Result<DaemonResponse, String> {
    let client = ApiClient::default();
    match request {
        DaemonRequest::DaemonPluginRequest { plugin_request } => {
            handle_plugin_request(&client, plugin_request)
        }
        DaemonRequest::ConnectionAdd {
            from_plugin,
            from_port,
            to_plugin,
            to_port,
            ..
        } => map_api(
            client.add_connection(
                stable_connection_id(*from_plugin, from_port, *to_plugin, to_port),
                *from_plugin as u32,
                stable_port_id(from_port),
                *to_plugin as u32,
                stable_port_id(to_port),
            ),
            "connection added",
        ),
        DaemonRequest::ConnectionRemove {
            from_plugin,
            from_port,
            to_plugin,
            to_port,
        } => map_api(
            client.remove_connection(stable_connection_id(
                *from_plugin,
                from_port,
                *to_plugin,
                to_port,
            )),
            "connection removed",
        ),
        DaemonRequest::ConnectionList | DaemonRequest::ConnectionShow { .. } => {
            Ok(DaemonResponse::ConnectionList {
                connections: Vec::<ConnectionSummary>::new(),
            })
        }
        DaemonRequest::ConnectionRemoveIndex { index } => {
            map_api(client.remove_connection((*index + 1) as u32), "connection removed")
        }
        DaemonRequest::DaemonStop => {
            let controller = DaemonController::new(DaemonConfig::default());
            controller
                .stop()
                .map(|message| DaemonResponse::Ok { message })
                .map_err(|error| error.to_string())
        }
        DaemonRequest::DaemonReload => {
            let controller = DaemonController::new(DaemonConfig::default());
            let _ = controller.stop();
            controller
                .start()
                .map(|message| DaemonResponse::Ok { message })
                .map_err(|error| error.to_string())
        }
        DaemonRequest::RuntimePluginStart { id } => map_api(
            client.transition_node(*id as u32, NodeState::Start),
            "plugin started",
        ),
        DaemonRequest::RuntimePluginStop { id } => map_api(
            client.transition_node(*id as u32, NodeState::Stop),
            "plugin stopped",
        ),
        DaemonRequest::RuntimePluginRestart { id } => map_api(
            client.transition_node(*id as u32, NodeState::Restart),
            "plugin restarted",
        ),
        DaemonRequest::RuntimePluginView { id } | DaemonRequest::RuntimeShow { id } => {
            let _ = client.subscribe_port_values(*id as u32, true, u64::MAX);
            let _ = client.subscribe_node_states(*id as u32, true, u64::MAX);
            Ok(DaemonResponse::RuntimePluginView {
                id: *id,
                kind: format!("node-{id}"),
                state: empty_state(),
                samples: Vec::new(),
                series_names: Vec::new(),
                period_seconds: 0.001,
                time_scale: 1.0,
                time_label: "s".to_string(),
            })
        }
        DaemonRequest::RuntimeSetVariables { id, json } => {
            let values: serde_json::Value =
                serde_json::from_str(json).map_err(|error| error.to_string())?;
            if let Some(map) = values.as_object() {
                for (name, value) in map {
                    let Some(rendered) = value_to_api_string(value) else {
                        continue;
                    };
                    let value_type = if value.is_string() {
                        ValueType::String
                    } else {
                        ValueType::F64
                    };
                    let _ = client.set_param(*id as u32, stable_port_id(name), value_type, &rendered);
                }
            }
            Ok(DaemonResponse::Ok {
                message: "variables updated".to_string(),
            })
        }
        DaemonRequest::RuntimeList => Ok(DaemonResponse::RuntimeList {
            plugins: Vec::new(),
        }),
        DaemonRequest::RuntimeSettingsOptions => Ok(DaemonResponse::RuntimeSettingsOptions {
            options: RuntimeSettingsOptions {
                frequency_units: vec!["hz".to_string(), "khz".to_string()],
                period_units: vec!["s".to_string(), "ms".to_string(), "us".to_string()],
                min_frequency_value: 1.0,
                min_period_value: 1.0,
                max_integration_steps_min: 1,
                max_integration_steps_max: 1_000_000,
            },
        }),
        DaemonRequest::RuntimeSettingsShow => Ok(DaemonResponse::RuntimeSettings {
            settings: workspace::WorkspaceSettings::default(),
        }),
        DaemonRequest::RuntimeSettingsSet { .. }
        | DaemonRequest::RuntimeSettingsSave
        | DaemonRequest::RuntimeSettingsRestore => Ok(DaemonResponse::Ok {
            message: "settings accepted by gui compatibility layer".to_string(),
        }),
        DaemonRequest::RuntimeUmlDiagram => Ok(DaemonResponse::RuntimeUmlDiagram {
            uml: "@startuml\n@enduml".to_string(),
        }),
        DaemonRequest::WorkspaceList => Ok(DaemonResponse::WorkspaceList {
            workspaces: Vec::new(),
        }),
        DaemonRequest::WorkspaceLoad { .. }
        | DaemonRequest::WorkspaceNew { .. }
        | DaemonRequest::WorkspaceSave { .. }
        | DaemonRequest::WorkspaceEdit { .. }
        | DaemonRequest::WorkspaceDelete { .. } => Ok(DaemonResponse::Ok {
            message: "workspace request handled locally by gui".to_string(),
        }),
    }
}

fn handle_plugin_request(
    client: &ApiClient,
    request: &PluginRequest,
) -> Result<DaemonResponse, String> {
    match request {
        PluginRequest::PluginList => Ok(DaemonResponse::PluginList {
            plugins: Vec::new(),
        }),
        PluginRequest::PluginInstall { path } => {
            let kind = infer_node_kind(path);
            map_api(client.load_node(kind, path), "node loaded")
        }
        PluginRequest::PluginAdd { name } => map_api(
            client.add_node(NodeKind::Plugin, name),
            "plugin added",
        )
        .map(|_| DaemonResponse::PluginAdded { id: stable_port_id(name) as u64 }),
        PluginRequest::PluginRemove { id } => map_api(
            client.transition_node(*id as u32, NodeState::Fini),
            "plugin removed",
        ),
        PluginRequest::PluginUninstall { name }
        | PluginRequest::PluginReinstall { name }
        | PluginRequest::PluginRebuild { name } => Ok(DaemonResponse::Ok {
            message: format!("plugin `{name}` handled by gui compatibility layer"),
        }),
    }
}

fn infer_node_kind(path: &str) -> NodeKind {
    if path.to_ascii_lowercase().contains("device") {
        NodeKind::Device
    } else {
        NodeKind::Plugin
    }
}

fn map_api(response: rtsyn_ui::Result<rtsyn_ui::api::ApiResponse>, message: &str) -> Result<DaemonResponse, String> {
    match response {
        Ok(response) if (200..300).contains(&response.status) => Ok(DaemonResponse::Ok {
            message: message.to_string(),
        }),
        Ok(response) => Ok(DaemonResponse::Error {
            message: format!("HTTP {}: {}", response.status, response.body),
        }),
        Err(error) => Ok(DaemonResponse::Error {
            message: error.to_string(),
        }),
    }
}

fn empty_state() -> RuntimePluginState {
    RuntimePluginState {
        outputs: Vec::new(),
        inputs: Vec::new(),
        internal_variables: Vec::new(),
        variables: Vec::new(),
    }
}

fn value_to_api_string(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::Number(number) => Some(number.to_string()),
        serde_json::Value::String(text) => Some(text.clone()),
        serde_json::Value::Bool(value) => Some(if *value { "1" } else { "0" }.to_string()),
        _ => None,
    }
}

fn stable_port_id(name: &str) -> u32 {
    stable_hash(name.as_bytes())
}

fn stable_connection_id(from_plugin: u64, from_port: &str, to_plugin: u64, to_port: &str) -> u32 {
    let input = format!("{from_plugin}:{from_port}>{to_plugin}:{to_port}");
    stable_hash(input.as_bytes())
}

fn stable_hash(bytes: &[u8]) -> u32 {
    let mut hash = 2_166_136_261u32;
    for byte in bytes {
        hash ^= u32::from(*byte);
        hash = hash.wrapping_mul(16_777_619);
    }
    hash.max(1)
}
