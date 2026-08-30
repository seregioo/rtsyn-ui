use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

pub mod execution;
pub use execution::{input_sum, input_sum_any, order_plugins_for_execution};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceDefinition {
    pub name: String,
    pub description: String,
    pub target_hz: u32,
    pub plugins: Vec<PluginDefinition>,
    pub connections: Vec<ConnectionDefinition>,
    #[serde(default)]
    pub settings: WorkspaceSettings,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceSettings {
    pub frequency_value: f64,
    pub frequency_unit: String,
    pub period_value: f64,
    pub period_unit: String,
    pub selected_cores: Vec<usize>,
    #[serde(default = "default_thread_priority")]
    pub thread_priority: i32,
    #[serde(default)]
    pub deadline_tolerance_ns: u64,
}

pub const DEFAULT_THREAD_PRIORITY: i32 = 99;

fn default_thread_priority() -> i32 {
    DEFAULT_THREAD_PRIORITY
}

impl Default for WorkspaceSettings {
    fn default() -> Self {
        Self {
            frequency_value: 1000.0,
            frequency_unit: "hz".to_string(),
            period_value: 1.0,
            period_unit: "ms".to_string(),
            selected_cores: vec![0],
            thread_priority: DEFAULT_THREAD_PRIORITY,
            deadline_tolerance_ns: 0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginDefinition {
    pub id: u64,
    pub kind: String,
    pub config: serde_json::Value,
    #[serde(default)]
    pub priority: i32,
    #[serde(default = "default_running", skip_serializing)]
    pub running: bool,
}

fn default_running() -> bool {
    false
}

fn sanitize_plugin_for_persistence(plugin: &mut PluginDefinition) {
    if let Some(map) = plugin.config.as_object_mut() {
        map.remove("running");
    }
}

fn sanitize_workspace_for_persistence(workspace: &mut WorkspaceDefinition) {
    let removed_plugin_ids = workspace
        .plugins
        .iter()
        .filter(|plugin| {
            plugin
                .config
                .get("api_managed")
                .and_then(|value| value.as_bool())
                .unwrap_or(false)
        })
        .map(|plugin| plugin.id)
        .collect::<std::collections::HashSet<_>>();
    if !removed_plugin_ids.is_empty() {
        workspace
            .plugins
            .retain(|plugin| !removed_plugin_ids.contains(&plugin.id));
        workspace.connections.retain(|connection| {
            !removed_plugin_ids.contains(&connection.from_plugin)
                && !removed_plugin_ids.contains(&connection.to_plugin)
        });
    }
    for plugin in &mut workspace.plugins {
        sanitize_plugin_for_persistence(plugin);
    }
}

fn sanitize_workspace_after_load(workspace: &mut WorkspaceDefinition) {
    sanitize_workspace_for_persistence(workspace);
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectionDefinition {
    pub from_plugin: u64,
    pub from_port: String,
    pub to_plugin: u64,
    pub to_port: String,
    pub kind: String,
}

#[derive(thiserror::Error, Debug)]
pub enum WorkspaceError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
}

#[derive(thiserror::Error, Debug, PartialEq, Eq)]
pub enum ConnectionRuleError {
    #[error("self connections are not allowed")]
    SelfConnection,
    #[error("input already has max connections")]
    InputLimitExceeded,
    #[error("connection between these plugins already exists")]
    DuplicateConnection,
}

pub fn validate_connection(
    connections: &[ConnectionDefinition],
    from_plugin: u64,
    to_plugin: u64,
    to_port: &str,
    max_per_input: usize,
) -> Result<(), ConnectionRuleError> {
    if from_plugin == to_plugin {
        return Err(ConnectionRuleError::SelfConnection);
    }
    let existing_count = connections
        .iter()
        .filter(|conn| conn.to_plugin == to_plugin && conn.to_port == to_port)
        .count();
    if existing_count >= max_per_input {
        return Err(ConnectionRuleError::InputLimitExceeded);
    }
    Ok(())
}

pub fn add_connection(
    connections: &mut Vec<ConnectionDefinition>,
    connection: ConnectionDefinition,
    max_per_input: usize,
) -> Result<(), ConnectionRuleError> {
    validate_connection(
        connections,
        connection.from_plugin,
        connection.to_plugin,
        &connection.to_port,
        max_per_input,
    )?;

    // Check if same output is already connected to a different input of the same target plugin
    if connections.iter().any(|conn| {
        conn.from_plugin == connection.from_plugin
            && conn.from_port == connection.from_port
            && conn.to_plugin == connection.to_plugin
            && conn.to_port != connection.to_port
    }) {
        return Err(ConnectionRuleError::DuplicateConnection);
    }

    connections.push(connection);
    Ok(())
}

pub fn prune_extendable_inputs_plugin_connections(
    connections: &mut Vec<ConnectionDefinition>,
    recorder_id: u64,
    input_count: usize,
) {
    connections.retain(|conn| {
        if conn.to_plugin != recorder_id {
            return true;
        }
        if conn.to_port == "in" {
            return input_count > 0;
        }
        let Some(index) = conn.to_port.strip_prefix("in_") else {
            return true;
        };
        index
            .parse::<usize>()
            .map(|idx| idx < input_count)
            .unwrap_or(true)
    });
}

pub fn remove_extendable_input(
    connections: &mut Vec<ConnectionDefinition>,
    plugin_id: u64,
    remove_idx: usize,
) -> bool {
    let mut changed = false;
    let mut updated = Vec::with_capacity(connections.len());
    for mut conn in connections.drain(..) {
        if conn.to_plugin != plugin_id {
            updated.push(conn);
            continue;
        }

        let input_idx = if conn.to_port == "in" {
            Some(0)
        } else {
            conn.to_port
                .strip_prefix("in_")
                .and_then(|value| value.parse::<usize>().ok())
        };
        match input_idx {
            Some(idx) if idx == remove_idx => {
                changed = true;
                continue;
            }
            Some(idx) if idx > remove_idx => {
                conn.to_port = format!("in_{}", idx - 1);
                updated.push(conn);
                changed = true;
            }
            _ => {
                updated.push(conn);
            }
        }
    }
    *connections = updated;
    changed
}

impl WorkspaceDefinition {
    pub fn save_to_file<P: AsRef<Path>>(&self, path: P) -> Result<(), WorkspaceError> {
        let mut sanitized = self.clone();
        sanitize_workspace_for_persistence(&mut sanitized);
        let data = serde_json::to_vec_pretty(&sanitized)?;
        fs::write(path, data)?;
        Ok(())
    }

    pub fn load_from_file<P: AsRef<Path>>(path: P) -> Result<Self, WorkspaceError> {
        let data = fs::read(path)?;
        let mut definition: WorkspaceDefinition = serde_json::from_slice(&data)?;
        sanitize_workspace_after_load(&mut definition);
        Ok(definition)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_settings_default_uses_realtime_priority() {
        assert_eq!(
            WorkspaceSettings::default().thread_priority,
            DEFAULT_THREAD_PRIORITY
        );
    }

    #[test]
    fn workspace_settings_missing_priority_deserializes_to_realtime_default() {
        let settings: WorkspaceSettings = serde_json::from_value(serde_json::json!({
            "frequency_value": 1000.0,
            "frequency_unit": "hz",
            "period_value": 1.0,
            "period_unit": "ms",
            "selected_cores": [0]
        }))
        .unwrap();

        assert_eq!(settings.thread_priority, DEFAULT_THREAD_PRIORITY);
    }

    #[test]
    fn save_omits_api_managed_runtime_nodes_and_connections() {
        let dir = std::env::temp_dir().join(format!(
            "rtsyn-workspace-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("workspace.json");

        let workspace = WorkspaceDefinition {
            name: "test".to_string(),
            description: String::new(),
            target_hz: 1000,
            plugins: vec![
                PluginDefinition {
                    id: 1,
                    kind: "live".to_string(),
                    config: serde_json::json!({"api_managed": true}),
                    priority: 0,
                    running: false,
                },
                PluginDefinition {
                    id: 2,
                    kind: "saved".to_string(),
                    config: serde_json::json!({}),
                    priority: 0,
                    running: false,
                },
            ],
            connections: vec![ConnectionDefinition {
                from_plugin: 1,
                from_port: "out".to_string(),
                to_plugin: 2,
                to_port: "in".to_string(),
                kind: "data".to_string(),
            }],
            settings: WorkspaceSettings::default(),
        };

        workspace.save_to_file(&path).unwrap();
        let loaded = WorkspaceDefinition::load_from_file(&path).unwrap();

        assert_eq!(loaded.plugins.len(), 1);
        assert_eq!(loaded.plugins[0].id, 2);
        assert!(loaded.connections.is_empty());

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn load_omits_stale_api_managed_runtime_nodes_and_connections() {
        let dir = std::env::temp_dir().join(format!(
            "rtsyn-workspace-stale-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("workspace.json");

        let data = serde_json::json!({
            "name": "test",
            "description": "",
            "target_hz": 1000,
            "plugins": [
                {
                    "id": 1,
                    "kind": "live",
                    "config": { "api_managed": true },
                    "priority": 0
                },
                {
                    "id": 2,
                    "kind": "saved",
                    "config": {},
                    "priority": 0
                }
            ],
            "connections": [
                {
                    "from_plugin": 1,
                    "from_port": "out",
                    "to_plugin": 2,
                    "to_port": "in",
                    "kind": "data"
                }
            ],
            "settings": WorkspaceSettings::default()
        });
        std::fs::write(&path, serde_json::to_vec_pretty(&data).unwrap()).unwrap();

        let loaded = WorkspaceDefinition::load_from_file(&path).unwrap();

        assert_eq!(loaded.plugins.len(), 1);
        assert_eq!(loaded.plugins[0].id, 2);
        assert!(loaded.connections.is_empty());

        let _ = std::fs::remove_dir_all(dir);
    }
}
