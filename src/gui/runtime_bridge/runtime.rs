use crate::api::{ApiClient, ApiResponse, NodeKind, NodeState, ValueType};
use crate::gui::runtime_bridge::message_handler::{
    LogicMessage, LogicState, RuntimeTelemetrySample,
};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::sync::mpsc::{Receiver, Sender};
use std::time::Duration;

const TELEMETRY_MAX_CATCH_UP_BYTES: u64 = 256 * 1024;
const TELEMETRY_MAX_PLOTTER_SAMPLES_PER_NODE: usize = 4096;

pub fn spawn_runtime() -> Result<(Sender<LogicMessage>, Receiver<LogicState>), String> {
    let (logic_tx, logic_rx) = std::sync::mpsc::channel::<LogicMessage>();
    let (logic_state_tx, logic_state_rx) = std::sync::mpsc::channel::<LogicState>();

    std::thread::Builder::new()
        .name("rtsyn-gui-api-runtime".to_string())
        .spawn(move || {
            let _ = run_runtime_loop(logic_rx, logic_state_tx);
        })
        .map_err(|error| error.to_string())?;

    Ok((logic_tx, logic_state_rx))
}

pub fn run_runtime_current(
    logic_rx: Receiver<LogicMessage>,
    logic_state_tx: Sender<LogicState>,
) -> Result<(), String> {
    run_runtime_loop(logic_rx, logic_state_tx)
}

fn run_runtime_loop(
    logic_rx: Receiver<LogicMessage>,
    logic_state_tx: Sender<LogicState>,
) -> Result<(), String> {
    let client = ApiClient::default();
    let mut telemetry_tail = TelemetryTail::default();
    let mut state = LogicState {
        outputs: HashMap::new(),
        input_values: HashMap::new(),
        internal_variable_values: HashMap::new(),
        viewer_values: HashMap::new(),
        tick: 0,
        plotter_samples: HashMap::new(),
        runtime_telemetry_samples: Vec::new(),
    };

    loop {
        let mut dirty = false;
        match logic_rx.recv_timeout(Duration::from_millis(33)) {
            Ok(message) => {
                handle_message(&client, message, &mut state);
                dirty = true;
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        }

        dirty |= telemetry_tail.drain(&client, &mut state);
        if dirty {
            state.tick = state.tick.wrapping_add(1);
            if logic_state_tx.send(state.clone()).is_err() {
                break;
            }
            state.runtime_telemetry_samples.clear();
        }
    }

    Ok(())
}

trait RuntimeApi {
    fn transition_node(&self, node_id: u32, state: NodeState) -> crate::Result<ApiResponse>;

    fn add_connection(
        &self,
        connection_id: u32,
        from_node_id: u32,
        from_port_id: u32,
        to_node_id: u32,
        to_port_id: u32,
    ) -> crate::Result<ApiResponse>;

    fn load_node(&self, kind: NodeKind, path: &str) -> crate::Result<ApiResponse>;

    fn set_param(
        &self,
        node_id: u32,
        param_id: u32,
        value_type: ValueType,
        value: &str,
    ) -> crate::Result<ApiResponse>;
}

impl RuntimeApi for ApiClient {
    fn transition_node(&self, node_id: u32, state: NodeState) -> crate::Result<ApiResponse> {
        ApiClient::transition_node(self, node_id, state)
    }

    fn add_connection(
        &self,
        connection_id: u32,
        from_node_id: u32,
        from_port_id: u32,
        to_node_id: u32,
        to_port_id: u32,
    ) -> crate::Result<ApiResponse> {
        ApiClient::add_connection(
            self,
            connection_id,
            from_node_id,
            from_port_id,
            to_node_id,
            to_port_id,
        )
    }

    fn load_node(&self, kind: NodeKind, path: &str) -> crate::Result<ApiResponse> {
        ApiClient::load_node(self, kind, path)
    }

    fn set_param(
        &self,
        node_id: u32,
        param_id: u32,
        value_type: ValueType,
        value: &str,
    ) -> crate::Result<ApiResponse> {
        ApiClient::set_param(self, node_id, param_id, value_type, value)
    }
}

#[derive(Default)]
struct TelemetryTail {
    path: Option<String>,
    offset: u64,
    initialized: bool,
}

impl TelemetryTail {
    fn drain(&mut self, client: &ApiClient, state: &mut LogicState) -> bool {
        if self.path.is_none() {
            self.path = client.telemetry_values_file().ok();
        }
        let Some(path) = self.path.as_deref() else {
            return false;
        };

        let Ok(mut file) = std::fs::File::open(path) else {
            return false;
        };
        let len = file.metadata().map(|metadata| metadata.len()).unwrap_or(0);
        if self.offset > len {
            self.offset = 0;
        }
        if !self.initialized {
            self.offset = len;
            self.initialized = true;
            return false;
        }
        if len.saturating_sub(self.offset) > TELEMETRY_MAX_CATCH_UP_BYTES {
            self.offset = len - TELEMETRY_MAX_CATCH_UP_BYTES;
        }
        if file.seek(SeekFrom::Start(self.offset)).is_err() {
            return false;
        }

        let mut reader = BufReader::new(file);
        let mut line = String::new();
        let mut dirty = false;
        loop {
            line.clear();
            let Ok(bytes) = reader.read_line(&mut line) else {
                break;
            };
            if bytes == 0 {
                break;
            }
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(line.trim()) {
                apply_telemetry_value(state, &value);
                dirty = true;
            }
        }
        self.offset = reader.stream_position().unwrap_or(self.offset);
        dirty
    }
}

fn apply_telemetry_value(state: &mut LogicState, line: &serde_json::Value) {
    let Some(node_id) = line.get("node_id").and_then(|value| value.as_u64()) else {
        return;
    };
    let Some(value_id) = line.get("value_id").and_then(|value| value.as_u64()) else {
        return;
    };
    let key = (node_id, value_id.to_string());
    let Some(value) = line.get("value") else {
        return;
    };
    let value_kind = line
        .get("kind")
        .and_then(|value| value.as_str())
        .unwrap_or("port");

    if let Some(number) = value.as_f64() {
        state.runtime_telemetry_samples.push(RuntimeTelemetrySample {
            node_id,
            value_id: value_id as u32,
            kind: value_kind.to_string(),
            cycle_id: line
                .get("cycle_id")
                .and_then(|value| value.as_u64())
                .unwrap_or(state.tick),
            timestamp_ns: line
                .get("timestamp_ns")
                .and_then(|value| value.as_u64())
                .unwrap_or_default(),
            value: number,
        });
        if value_kind == "state" {
            state
                .internal_variable_values
                .insert(key, serde_json::Value::from(number));
        } else {
            state.outputs.insert(key.clone(), number);
            state.input_values.insert(key.clone(), number);
            state.viewer_values.insert(node_id, number);
            let sample = state.plotter_samples.entry(node_id).or_default();
            let tick = line
                .get("cycle_id")
                .and_then(|value| value.as_u64())
                .unwrap_or(state.tick);
            if sample.last().map(|(last_tick, _)| *last_tick) != Some(tick) {
                sample.push((tick, vec![number]));
            } else if let Some((_, values)) = sample.last_mut() {
                values.push(number);
            }
            if sample.len() > TELEMETRY_MAX_PLOTTER_SAMPLES_PER_NODE {
                let excess = sample.len() - TELEMETRY_MAX_PLOTTER_SAMPLES_PER_NODE;
                sample.drain(0..excess);
            }
        }
        return;
    }

    if value_kind == "state" {
        state.internal_variable_values.insert(key, value.clone());
    }
}

fn handle_message(client: &impl RuntimeApi, message: LogicMessage, state: &mut LogicState) {
    match message {
        LogicMessage::UpdateSettings(_) => {}
        LogicMessage::UpdateWorkspace(workspace) => {
            for plugin in workspace.plugins {
                let node_id = plugin.id as u32;
                if plugin.running {
                    let _ = client.transition_node(node_id, NodeState::Start);
                }
            }
            for connection in workspace.connections {
                let _ = client.add_connection(
                    stable_connection_id(
                        connection.from_plugin,
                        &connection.from_port,
                        connection.to_plugin,
                        &connection.to_port,
                    ),
                    connection.from_plugin as u32,
                    stable_port_id(&connection.from_port),
                    connection.to_plugin as u32,
                    stable_port_id(&connection.to_port),
                );
            }
        }
        LogicMessage::SetPluginRunning(id, running) => {
            let next = if running {
                NodeState::Start
            } else {
                NodeState::Stop
            };
            let _ = client.transition_node(id as u32, next);
        }
        LogicMessage::SetAllPluginsRunning(_) => {}
        LogicMessage::RestartPlugin(id) => {
            let _ = client.transition_node(id as u32, NodeState::Restart);
        }
        LogicMessage::QueryPluginBehavior(_, _, tx) => {
            let _ = tx.send(Some(crate::gui::tool_api::ui::PluginBehavior::default()));
        }
        LogicMessage::QueryPluginMetadata(path, tx) => {
            let _ = client.load_node(NodeKind::Plugin, &path);
            let _ = tx.send(Some((Vec::new(), Vec::new(), Vec::new(), None, None)));
        }
        LogicMessage::GetPluginVariable(id, name, tx) => {
            let value = state.internal_variable_values.get(&(id, name)).cloned();
            let _ = tx.send(value);
        }
        LogicMessage::SetPluginVariable(id, name, value) => {
            state
                .internal_variable_values
                .insert((id, name.clone()), value.clone());
            if let Some(value) = value_to_api_string(&value) {
                let _ = client.set_param(id as u32, stable_port_id(&name), ValueType::F64, &value);
            }
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gui::workspace_model::{
        ConnectionDefinition, PluginDefinition, WorkspaceDefinition, WorkspaceSettings,
    };
    use std::cell::RefCell;
    use std::io::Write;

    fn empty_state() -> LogicState {
        LogicState {
            outputs: HashMap::new(),
            input_values: HashMap::new(),
            internal_variable_values: HashMap::new(),
            viewer_values: HashMap::new(),
            tick: 0,
            plotter_samples: HashMap::new(),
            runtime_telemetry_samples: Vec::new(),
        }
    }

    #[derive(Default)]
    struct MockRuntimeApi {
        transitions: RefCell<Vec<(u32, NodeState)>>,
        connections: RefCell<Vec<(u32, u32, u32, u32, u32)>>,
        loads: RefCell<Vec<(NodeKind, String)>>,
        params: RefCell<Vec<(u32, u32, ValueType, String)>>,
    }

    impl MockRuntimeApi {
        fn ok_response() -> crate::Result<ApiResponse> {
            Ok(ApiResponse {
                status: 200,
                body: String::new(),
            })
        }
    }

    impl RuntimeApi for MockRuntimeApi {
        fn transition_node(&self, node_id: u32, state: NodeState) -> crate::Result<ApiResponse> {
            self.transitions.borrow_mut().push((node_id, state));
            Self::ok_response()
        }

        fn add_connection(
            &self,
            connection_id: u32,
            from_node_id: u32,
            from_port_id: u32,
            to_node_id: u32,
            to_port_id: u32,
        ) -> crate::Result<ApiResponse> {
            self.connections.borrow_mut().push((
                connection_id,
                from_node_id,
                from_port_id,
                to_node_id,
                to_port_id,
            ));
            Self::ok_response()
        }

        fn load_node(&self, kind: NodeKind, path: &str) -> crate::Result<ApiResponse> {
            self.loads.borrow_mut().push((kind, path.to_string()));
            Self::ok_response()
        }

        fn set_param(
            &self,
            node_id: u32,
            param_id: u32,
            value_type: ValueType,
            value: &str,
        ) -> crate::Result<ApiResponse> {
            self.params
                .borrow_mut()
                .push((node_id, param_id, value_type, value.to_string()));
            Self::ok_response()
        }
    }

    #[test]
    fn workspace_update_does_not_create_runtime_nodes() {
        let client = MockRuntimeApi::default();
        let mut state = empty_state();
        let workspace = WorkspaceDefinition {
            name: "test".to_string(),
            description: String::new(),
            target_hz: 1000,
            plugins: vec![
                PluginDefinition {
                    id: 7,
                    kind: "adder".to_string(),
                    config: serde_json::json!({"api_managed": true}),
                    priority: 0,
                    running: true,
                },
                PluginDefinition {
                    id: 9,
                    kind: "forwarder".to_string(),
                    config: serde_json::json!({"api_managed": true}),
                    priority: 0,
                    running: false,
                },
            ],
            connections: vec![ConnectionDefinition {
                from_plugin: 9,
                from_port: "out".to_string(),
                to_plugin: 7,
                to_port: "left".to_string(),
                kind: "same_cycle".to_string(),
            }],
            settings: WorkspaceSettings::default(),
        };

        handle_message(
            &client,
            LogicMessage::UpdateWorkspace(workspace),
            &mut state,
        );

        assert_eq!(
            client.transitions.borrow().as_slice(),
            &[(7, NodeState::Start)]
        );
        assert_eq!(client.connections.borrow().len(), 1);
        assert!(client.loads.borrow().is_empty());
        assert!(client.params.borrow().is_empty());
    }

    #[test]
    fn telemetry_tail_skips_existing_file_and_reads_appended_samples() {
        let path = std::env::temp_dir().join(format!(
            "rtsyn-gui-telemetry-tail-{}.jsonl",
            std::process::id()
        ));
        let mut file = std::fs::File::create(&path).expect("create telemetry fixture");
        writeln!(
            file,
            r#"{{"cycle_id":1,"node_id":4,"value_id":0,"value":12.0}}"#
        )
        .expect("write stale telemetry");
        drop(file);

        let client = ApiClient::default();
        let mut state = empty_state();
        let mut tail = TelemetryTail {
            path: Some(path.to_string_lossy().into_owned()),
            offset: 0,
            initialized: false,
        };

        tail.drain(&client, &mut state);
        assert!(state.outputs.is_empty());

        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .expect("open telemetry fixture");
        writeln!(
            file,
            r#"{{"cycle_id":2,"node_id":4,"value_id":0,"value":21.0}}"#
        )
        .expect("write live telemetry");
        drop(file);

        tail.drain(&client, &mut state);
        assert_eq!(state.outputs.get(&(4, "0".to_string())), Some(&21.0));

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn telemetry_value_kind_routes_numeric_states_separately_from_ports() {
        let mut state = empty_state();
        let port = serde_json::json!({
            "cycle_id": 1,
            "node_id": 7,
            "value_id": 0,
            "kind": "port",
            "value": 3.0
        });
        let node_state = serde_json::json!({
            "cycle_id": 1,
            "node_id": 7,
            "value_id": 0,
            "kind": "state",
            "value": 9.0
        });

        apply_telemetry_value(&mut state, &port);
        apply_telemetry_value(&mut state, &node_state);

        assert_eq!(state.outputs.get(&(7, "0".to_string())), Some(&3.0));
        assert_eq!(
            state.internal_variable_values.get(&(7, "0".to_string())),
            Some(&serde_json::Value::from(9.0))
        );
    }
}
