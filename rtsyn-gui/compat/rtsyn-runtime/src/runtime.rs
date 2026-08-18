use crate::message_handler::{LogicMessage, LogicState};
use rtsyn_ui::api::{ApiClient, NodeKind, NodeState, ValueType};
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
        }
    }

    Ok(())
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

fn handle_message(client: &ApiClient, message: LogicMessage, state: &mut LogicState) {
    match message {
        LogicMessage::UpdateSettings(_) => {}
        LogicMessage::UpdateWorkspace(workspace) => {
            for plugin in workspace.plugins {
                let _ = client.add_node(NodeKind::Plugin, &plugin.kind);
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
            let next = if running { NodeState::Start } else { NodeState::Stop };
            let _ = client.transition_node(id as u32, next);
        }
        LogicMessage::SetAllPluginsRunning(_) => {}
        LogicMessage::RestartPlugin(id) => {
            let _ = client.transition_node(id as u32, NodeState::Restart);
        }
        LogicMessage::QueryPluginBehavior(_, _, tx) => {
            let _ = tx.send(Some(rtsyn_plugin::ui::PluginBehavior::default()));
        }
        LogicMessage::QueryPluginMetadata(path, tx) => {
            let _ = client.load_node(NodeKind::Plugin, &path);
            let _ = tx.send(Some((Vec::new(), Vec::new(), Vec::new(), None, None)));
        }
        LogicMessage::GetPluginVariable(id, name, tx) => {
            let value = state
                .internal_variable_values
                .get(&(id, name))
                .cloned();
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
    use std::io::Write;

    fn empty_state() -> LogicState {
        LogicState {
            outputs: HashMap::new(),
            input_values: HashMap::new(),
            internal_variable_values: HashMap::new(),
            viewer_values: HashMap::new(),
            tick: 0,
            plotter_samples: HashMap::new(),
        }
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
