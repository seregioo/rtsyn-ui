use crate::gui::runtime_bridge::LogicMessage;
use crate::gui::state::PluginOrderMode;
use crate::gui::tool_api::ui::{DisplaySchema, FieldType, UIField, UISchema};
use crate::gui::tool_model::plugin::{
    InstalledPlugin, PluginManager, PluginManifest, PluginMetadataSource,
};
use crate::gui::utils::format_f64_with_input;
use crate::gui::GuiApp;
use crate::gui::HighlightMode;
use crate::gui::RuntimeNodeAddResult;
use crate::gui::ViewMode;
use rtsyn_ui::api::{ApiClient, ApiResponse, NodeKind, NodeState, ValueType};
use rtsyn_ui::daemon::DaemonController;
use rtsyn_ui::metadata::{ControlKind, PluginControl, PluginUiMetadata};
use rtsyn_ui::module::{build_runtime_module, rebuild_runtime_module, runtime_module_root};
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

/// GUI implementation of plugin metadata source that communicates with the runtime logic thread.
///
/// This struct provides a bridge between the plugin manager and the runtime system,
/// allowing the GUI to query plugin metadata and behavior information through
/// message passing to the logic thread.
struct GuiMetadataSource<'a> {
    /// Channel sender for communicating with the runtime logic thread
    logic_tx: &'a mpsc::Sender<LogicMessage>,
}

fn runtime_node_kind_label(kind: NodeKind) -> &'static str {
    match kind {
        NodeKind::Plugin => "plugin",
        NodeKind::Device => "device",
    }
}

fn queue_runtime_node_add_request(
    pending: &mut Option<(NodeKind, String)>,
    in_progress: bool,
    kind: NodeKind,
    node_name: &str,
) -> bool {
    if pending.is_some() || in_progress {
        return false;
    }
    *pending = Some((kind, node_name.trim().to_string()));
    true
}

fn node_kind_from_metadata(metadata: &RuntimeNodeMetadata) -> NodeKind {
    match metadata.node_type.as_str() {
        "device" => NodeKind::Device,
        _ => NodeKind::Plugin,
    }
}

fn runtime_state_is_running(state: &str) -> bool {
    matches!(state, "start" | "process" | "restart" | "running")
}

fn runtime_nodes_snapshot_http_error(response: &ApiResponse) -> String {
    if response.status == 404 {
        return "Running daemon does not expose runtime node snapshots. Restart daemon to preserve state.".to_string();
    }

    format!(
        "Runtime node snapshot failed: HTTP {} {}",
        response.status, response.body
    )
}

fn remember_recent(list: &mut Vec<String>, value: &str) {
    let value = value.trim();
    if value.is_empty() {
        return;
    }
    list.retain(|entry| entry != value);
    list.insert(0, value.to_string());
    list.truncate(12);
}

fn normalized_lookup_key(value: &str) -> String {
    value.trim().to_lowercase()
}

fn runtime_node_kind_aliases(installed_plugins: &[InstalledPlugin]) -> HashMap<String, String> {
    let mut aliases = HashMap::new();
    for plugin in installed_plugins {
        let kind = plugin.manifest.kind.clone();
        for alias in [
            plugin.manifest.kind.clone(),
            plugin.manifest.name.clone(),
            PluginManager::display_kind(&plugin.manifest.kind),
        ] {
            aliases
                .entry(normalized_lookup_key(&alias))
                .or_insert_with(|| kind.clone());
        }
    }
    aliases
}

fn canonical_runtime_node_kind_for(
    installed_plugins: &[InstalledPlugin],
    kind_or_name: &str,
) -> Option<String> {
    let aliases = runtime_node_kind_aliases(installed_plugins);
    aliases.get(&normalized_lookup_key(kind_or_name)).cloned()
}

fn workspace_node_has_runtime_descriptor(
    installed_plugins: &[InstalledPlugin],
    kind_or_name: &str,
) -> bool {
    canonical_runtime_node_kind_for(installed_plugins, kind_or_name).is_some()
}

fn runtime_module_descriptor_name(module_path: &str) -> String {
    let path = Path::new(module_path);
    if let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) {
        let without_lib = stem.strip_prefix("lib").unwrap_or(stem);
        let without_project = without_lib
            .strip_prefix("rtsyn-")
            .or_else(|| without_lib.strip_prefix("rtsyn_"))
            .unwrap_or(without_lib);
        let name = without_project.replace('-', "_");
        if !name.is_empty() && path.file_name().and_then(|name| name.to_str()) != Some("xmake.lua")
        {
            return name;
        }
    }
    if path.file_name().and_then(|name| name.to_str()) == Some("xmake.lua") {
        if let Some(parent) = path.parent().and_then(|parent| parent.file_name()) {
            if let Some(parent) = parent.to_str() {
                return parent
                    .strip_prefix("rtsyn-")
                    .or_else(|| parent.strip_prefix("rtsyn_"))
                    .unwrap_or(parent)
                    .replace('-', "_");
            }
        }
    }
    module_path.to_string()
}

fn mark_runtime_params_applied(
    map: &mut serde_json::Map<String, Value>,
    params: &[(u32, String, String, ValueType)],
) {
    let applied = map
        .entry("_applied_params".to_string())
        .or_insert_with(|| Value::Object(serde_json::Map::new()));
    let Some(applied) = applied.as_object_mut() else {
        *applied = Value::Object(serde_json::Map::new());
        let Some(applied) = applied.as_object_mut() else {
            return;
        };
        for (_, name, value, value_type) in params {
            applied.insert(name.clone(), runtime_param_json_value(*value_type, value));
        }
        return;
    };
    for (_, name, value, value_type) in params {
        applied.insert(name.clone(), runtime_param_json_value(*value_type, value));
    }
}

fn runtime_param_json_value(value_type: ValueType, value: &str) -> Value {
    match value_type {
        ValueType::String => Value::String(value.to_string()),
        ValueType::I64 => value.parse::<i64>().map(Value::from).unwrap_or(Value::Null),
        ValueType::U64 => value.parse::<u64>().map(Value::from).unwrap_or(Value::Null),
        ValueType::F32 | ValueType::F64 => {
            value.parse::<f64>().map(Value::from).unwrap_or(Value::Null)
        }
    }
}

fn value_type_for_field(field: &crate::gui::tool_api::ui::UIField) -> ValueType {
    field
        .value_type
        .as_deref()
        .and_then(|value_type| ValueType::parse(value_type).ok())
        .unwrap_or_else(|| match &field.field_type {
            crate::gui::tool_api::ui::FieldType::Text { .. }
            | crate::gui::tool_api::ui::FieldType::FilePath { .. }
            | crate::gui::tool_api::ui::FieldType::Choice { .. } => ValueType::String,
            crate::gui::tool_api::ui::FieldType::Integer { .. }
            | crate::gui::tool_api::ui::FieldType::Boolean => ValueType::U64,
            crate::gui::tool_api::ui::FieldType::Float { .. }
            | crate::gui::tool_api::ui::FieldType::DynamicList { .. } => ValueType::F64,
        })
}

fn runtime_param_text_value(value: &Value, value_type: ValueType) -> String {
    match value_type {
        ValueType::String => value.as_str().unwrap_or("").to_string(),
        ValueType::I64 | ValueType::U64 => value
            .as_i64()
            .map(|value| value.to_string())
            .or_else(|| value.as_u64().map(|value| value.to_string()))
            .or_else(|| value.as_f64().map(|value| (value as i64).to_string()))
            .or_else(|| value.as_bool().map(|value| u8::from(value).to_string()))
            .unwrap_or_else(|| "0".to_string()),
        ValueType::F32 | ValueType::F64 => {
            if let Some(value) = value.as_f64() {
                value.to_string()
            } else {
                "0".to_string()
            }
        }
    }
}

fn runtime_node_default_param_assignments(
    installed_plugins: &[InstalledPlugin],
    node_name: &str,
    metadata: &RuntimeNodeMetadata,
) -> Vec<(u32, String, String, ValueType)> {
    let installed = installed_plugins
        .iter()
        .find(|plugin| plugin.manifest.kind == node_name);
    metadata
        .params
        .iter()
        .filter_map(|param| {
            let descriptor_type =
                ValueType::parse(param.value_type.as_str()).unwrap_or(ValueType::F64);
            if let Some(field) = installed
                .and_then(|plugin| plugin.ui_schema.as_ref())
                .and_then(|schema| {
                    schema
                        .fields
                        .iter()
                        .find(|field| field.key == param.name || field.name == param.name)
                })
            {
                let value_type = value_type_for_field(field);
                let default = field.default.as_ref()?;
                return Some((
                    param.id,
                    param.name.clone(),
                    runtime_param_text_value(default, value_type),
                    value_type,
                ));
            }

            if !param.default.trim().is_empty() {
                return Some((
                    param.id,
                    param.name.clone(),
                    param.default.clone(),
                    descriptor_type,
                ));
            }

            installed
                .and_then(|plugin| {
                    plugin
                        .metadata_variables
                        .iter()
                        .find(|(name, _)| name == &param.name)
                })
                .map(|(_, default)| {
                    (
                        param.id,
                        param.name.clone(),
                        default.to_string(),
                        ValueType::F64,
                    )
                })
        })
        .collect()
}

#[derive(Debug, Deserialize)]
struct RuntimeNodesSnapshot {
    #[serde(default)]
    nodes: Vec<RuntimeCommandResult>,
    #[serde(default)]
    loaded_descriptors: Vec<RuntimeCommandResult>,
    #[serde(default)]
    connections: Vec<RuntimeConnectionSnapshot>,
    #[serde(default)]
    param_values: Vec<RuntimeParamSnapshot>,
    #[serde(default)]
    latest_values: Vec<RuntimeLatestValueSnapshot>,
}

#[derive(Clone, Debug, Deserialize)]
struct RuntimeNodeMetadata {
    name: String,
    #[serde(default)]
    node_type: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    ports: Vec<RuntimeNodePortMetadata>,
    #[serde(default)]
    params: Vec<RuntimeNodeParamMetadata>,
    #[serde(default)]
    states: Vec<RuntimeNodeStateMetadata>,
}

#[derive(Clone, Debug, Deserialize)]
struct RuntimeCommandResult {
    #[serde(default = "invalid_runtime_node_id")]
    node_id: u32,
    #[serde(default)]
    runtime_state: String,
    node: RuntimeNodeMetadata,
}

#[derive(Debug, Deserialize)]
struct RuntimeCommandStatus {
    #[serde(default)]
    status_code: u32,
}

#[derive(Clone, Debug, Deserialize)]
struct RuntimeNodePortMetadata {
    #[serde(default)]
    id: u32,
    name: String,
    direction: String,
}

#[derive(Clone, Debug, Deserialize)]
struct RuntimeNodeParamMetadata {
    #[serde(default)]
    id: u32,
    name: String,
    #[serde(default)]
    default: String,
    #[serde(default = "default_runtime_value_type")]
    value_type: String,
    #[serde(default)]
    description: String,
}

#[derive(Clone, Debug, Deserialize)]
struct RuntimeNodeStateMetadata {
    #[serde(default)]
    id: u32,
    name: String,
}

#[derive(Clone, Debug, Deserialize)]
struct RuntimeConnectionSnapshot {
    connection_id: u32,
    source_node_id: u32,
    source_port_id: u32,
    destination_node_id: u32,
    destination_port_id: u32,
}

#[derive(Clone, Debug, Deserialize)]
struct RuntimeParamSnapshot {
    node_id: u32,
    param_id: u32,
    value: serde_json::Value,
}

#[derive(Clone, Debug, Deserialize)]
struct RuntimeLatestValueSnapshot {
    node_id: u32,
    value_id: u32,
    kind: String,
    value: serde_json::Value,
}

impl PluginMetadataSource for GuiMetadataSource<'_> {
    /// Queries plugin metadata from the runtime logic thread.
    ///
    /// # Parameters
    /// - `library_path`: Path to the plugin library file
    /// - `timeout`: Maximum time to wait for response
    ///
    /// # Returns
    /// Optional tuple containing:
    /// - Input port names
    /// - Output port names  
    /// - Variable definitions (name, default value)
    /// - Display schema for UI rendering
    /// - UI schema for configuration
    ///
    /// # Side Effects
    /// Sends message to logic thread and blocks waiting for response
    fn query_plugin_metadata(
        &self,
        library_path: &str,
        timeout: Duration,
    ) -> Option<(
        Vec<String>,
        Vec<String>,
        Vec<(String, f64)>,
        Option<crate::gui::tool_api::ui::DisplaySchema>,
        Option<crate::gui::tool_api::ui::UISchema>,
    )> {
        let (tx, rx) = mpsc::channel();
        let _ = self.logic_tx.send(LogicMessage::QueryPluginMetadata(
            library_path.to_string(),
            tx,
        ));
        rx.recv_timeout(timeout).ok().flatten()
    }

    /// Queries plugin behavior information from the runtime.
    ///
    /// # Parameters
    /// - `kind`: Plugin type identifier
    /// - `library_path`: Optional path to plugin library
    /// - `timeout`: Maximum time to wait for response
    ///
    /// # Returns
    /// Optional plugin behavior configuration
    ///
    /// # Side Effects
    /// Sends message to logic thread and blocks waiting for response
    fn query_plugin_behavior(
        &self,
        kind: &str,
        library_path: Option<&str>,
        timeout: Duration,
    ) -> Option<crate::gui::tool_api::ui::PluginBehavior> {
        let (tx, rx) = mpsc::channel();
        let _ = self.logic_tx.send(LogicMessage::QueryPluginBehavior(
            kind.to_string(),
            library_path.map(|s| s.to_string()),
            tx,
        ));
        rx.recv_timeout(timeout).ok().flatten()
    }
}

fn invalid_runtime_node_id() -> u32 {
    u32::MAX
}

fn default_runtime_value_type() -> String {
    "f64".to_string()
}

fn mask_from_display_entries(entries: &[String]) -> u64 {
    let mut mask = 0_u64;
    for entry in entries {
        let key = entry
            .split_once('|')
            .map(|(key, _)| key.trim())
            .unwrap_or_else(|| entry.trim());
        if let Ok(id) = key.parse::<u32>() {
            if id < 64 {
                mask |= 1_u64 << id;
            }
        }
    }
    mask
}

impl GuiApp {
    pub(crate) const PLUGIN_CARD_WIDTH: f32 = 316.0;
    pub(crate) const PLUGIN_CARD_FIXED_HEIGHT: f32 = 132.0;
    pub(crate) const PLUGIN_CARD_GRID_X_GAP: f32 = 20.0;
    pub(crate) const PLUGIN_CARD_GRID_Y_GAP: f32 = 48.0;
    pub(crate) const PLUGIN_LAYOUT_OFFSET_X: f32 = 12.0;
    pub(crate) const PLUGIN_LAYOUT_OFFSET_Y: f32 = 12.0;

    pub(crate) fn plugin_layout_grid_for_view(view_mode: ViewMode) -> (f32, f32, f32, f32) {
        match view_mode {
            // Keep spacing aligned with card dimensions and avoid overlap.
            ViewMode::Cards => (
                Self::PLUGIN_CARD_WIDTH + Self::PLUGIN_CARD_GRID_X_GAP,
                Self::PLUGIN_CARD_FIXED_HEIGHT + Self::PLUGIN_CARD_GRID_Y_GAP,
                Self::PLUGIN_LAYOUT_OFFSET_X,
                Self::PLUGIN_LAYOUT_OFFSET_Y,
            ),
            ViewMode::State => (100.0, 100.0, 50.0, 50.0),
        }
    }

    pub(crate) fn order_plugins_layout(
        &mut self,
        panel_rect: eframe::egui::Rect,
        mode: PluginOrderMode,
    ) {
        let mut plugin_ids: Vec<u64> = self
            .workspace_manager
            .workspace
            .plugins
            .iter()
            .filter(|plugin| {
                !self
                    .behavior_manager
                    .cached_behaviors
                    .get(&plugin.kind)
                    .map(|b| b.external_window)
                    .unwrap_or(false)
            })
            .map(|plugin| plugin.id)
            .collect();

        let name_by_kind = self.get_name_by_kind();
        let by_id: HashMap<u64, (String, i32)> = self
            .workspace_manager
            .workspace
            .plugins
            .iter()
            .map(|plugin| {
                let name = name_by_kind
                    .get(&plugin.kind)
                    .cloned()
                    .unwrap_or_else(|| Self::display_kind(&plugin.kind));
                (plugin.id, (name, plugin.priority))
            })
            .collect();
        let mut connection_counts: HashMap<u64, usize> = HashMap::new();
        for conn in &self.workspace_manager.workspace.connections {
            *connection_counts.entry(conn.from_plugin).or_insert(0) += 1;
            *connection_counts.entry(conn.to_plugin).or_insert(0) += 1;
        }

        plugin_ids.sort_by(|left, right| match mode {
            PluginOrderMode::Name => {
                let left_name = by_id
                    .get(left)
                    .map(|v| v.0.as_str())
                    .unwrap_or_default()
                    .to_ascii_lowercase();
                let right_name = by_id
                    .get(right)
                    .map(|v| v.0.as_str())
                    .unwrap_or_default()
                    .to_ascii_lowercase();
                left_name.cmp(&right_name).then_with(|| left.cmp(right))
            }
            PluginOrderMode::Id => left.cmp(right),
            PluginOrderMode::Priority => {
                let left_priority = by_id.get(left).map(|v| v.1).unwrap_or(0);
                let right_priority = by_id.get(right).map(|v| v.1).unwrap_or(0);
                left_priority
                    .cmp(&right_priority)
                    .then_with(|| left.cmp(right))
            }
            PluginOrderMode::Connections => {
                let left_connections = connection_counts.get(left).copied().unwrap_or(0);
                let right_connections = connection_counts.get(right).copied().unwrap_or(0);
                right_connections
                    .cmp(&left_connections)
                    .then_with(|| left.cmp(right))
            }
        });

        let (x_step, y_step, x_offset, y_offset) =
            Self::plugin_layout_grid_for_view(self.view_mode);
        let usable_width = (panel_rect.width() - x_offset * 2.0).max(x_step);
        let cols = ((usable_width / x_step).floor() as usize).max(1);

        for (idx, plugin_id) in plugin_ids.iter().enumerate() {
            let col = idx % cols;
            let row = idx / cols;
            let pos = panel_rect.min
                + eframe::egui::vec2(
                    x_offset + (col as f32 * x_step),
                    y_offset + (row as f32 * y_step),
                );
            match self.view_mode {
                ViewMode::Cards => {
                    self.plugin_positions.insert(*plugin_id, pos);
                }
                ViewMode::State => {
                    self.state_plugin_positions.insert(*plugin_id, pos);
                }
            }
        }
    }

    /// Drains plugin compatibility warnings and shows them as notifications.
    ///
    /// # Side Effects
    /// - Retrieves warnings from plugin manager
    /// - Adds new warnings to seen warnings set
    /// - Shows info notifications for unseen warnings
    fn drain_plugin_compatibility_warnings_to_notifications(&mut self) {
        for warning in self.plugin_manager.take_compatibility_warnings() {
            if self.seen_compatibility_warnings.insert(warning.clone()) {
                self.show_info("Plugin Compatibility", &warning);
            }
        }
    }

    /// Scans for detected plugins in standard directories.
    ///
    /// # Side Effects
    /// - Scans "plugins" and "app_plugins" directories
    /// - Updates plugin manager's detected plugins list
    /// - Shows compatibility warnings as notifications
    pub(crate) fn scan_detected_plugins(&mut self) {
        self.plugin_manager
            .scan_detected_plugins_in(&["plugins", "app_plugins"]);
        self.drain_plugin_compatibility_warnings_to_notifications();
    }

    pub(crate) fn load_runtime_plugin_module(&mut self, module_path: &str) {
        self.load_runtime_node_module(NodeKind::Plugin, module_path);
    }

    pub(crate) fn load_runtime_device_module(&mut self, module_path: &str) {
        self.load_runtime_node_module(NodeKind::Device, module_path);
    }

    pub(crate) fn recompile_runtime_node_module(&mut self, kind: NodeKind, module_path: &str) {
        let label = runtime_node_kind_label(kind);
        match rebuild_runtime_module(module_path.trim()) {
            Ok(_) => self.load_runtime_node_module(kind, module_path),
            Err(error) => {
                self.status = format!("Recompile {label} failed: {error}");
                self.show_info("Runtime node", &self.status.clone());
            }
        }
    }

    pub(crate) fn forget_runtime_node_module(&mut self, kind: NodeKind, module_path: &str) {
        let list = match kind {
            NodeKind::Plugin => &mut self.windows.recent_plugin_modules,
            NodeKind::Device => &mut self.windows.recent_device_modules,
        };
        list.retain(|path| path != module_path);
        match kind {
            NodeKind::Plugin if self.windows.load_plugin_path == module_path => {
                self.windows.load_plugin_path.clear();
            }
            NodeKind::Device if self.windows.load_device_path == module_path => {
                self.windows.load_device_path.clear();
            }
            _ => {}
        }
        self.windows.runtime_node_selected_index = None;
        self.status = format!("Removed remembered {} module", runtime_node_kind_label(kind));
        self.persist_gui_session();
    }

    fn load_runtime_node_module(&mut self, kind: NodeKind, module_path: &str) {
        let module_path = module_path.trim();
        if module_path.is_empty() {
            self.status = "Module path is required".to_string();
            self.show_info("Runtime node", "Module path is required");
            return;
        }

        let label = runtime_node_kind_label(kind);
        let module = match build_runtime_module(module_path) {
            Ok(module) => module,
            Err(error) => {
                self.status = format!("Load {label} failed: {error}");
                self.show_info("Runtime node", &self.status.clone());
                return;
            }
        };
        let module_root = module.module_root;
        let module_path_text = module.shared_library.to_string_lossy().to_string();

        match load_node_with_daemon_retry(kind, &module_path_text) {
            Ok(response) if (200..300).contains(&response.status) => {
                if let Some(metadata) = runtime_node_metadata_from_response(&response) {
                    let node_name = metadata.name.clone();
                    self.register_runtime_module_metadata(
                        kind,
                        metadata,
                        module_root.clone(),
                        None,
                    );
                    self.set_runtime_node_name_if_empty_or_inferred(kind, &node_name);
                    self.remember_runtime_node_name(kind, &node_name);
                    self.status = format!("{label} module loaded: {module_path}");
                    self.remember_runtime_module(kind, module_path);
                    self.persist_gui_session();
                    self.show_info("Runtime node", &self.status.clone());
                } else {
                    self.status = format!("Load {label} pending: descriptor not available yet");
                    self.show_info(
                        "Runtime node",
                        &format!("{} {}", self.status, response.body),
                    );
                }
            }
            Ok(response) => {
                self.status = format!("Load {label} failed: HTTP {}", response.status);
                self.show_info(
                    "Runtime node",
                    &format!("{} {}", self.status, response.body),
                );
            }
            Err(error) => {
                self.status = format!("Load {label} failed: {error}");
                self.show_info("Runtime node", &self.status.clone());
            }
        }
    }

    pub(crate) fn add_runtime_plugin_node(&mut self, node_name: &str) {
        self.queue_runtime_node_add(NodeKind::Plugin, node_name);
    }

    pub(crate) fn add_runtime_device_node(&mut self, node_name: &str) {
        self.queue_runtime_node_add(NodeKind::Device, node_name);
    }

    pub(crate) fn queue_runtime_node_add(&mut self, kind: NodeKind, node_name: &str) {
        let _ = queue_runtime_node_add_request(
            &mut self.pending_runtime_node_add,
            self.runtime_node_add_in_progress,
            kind,
            node_name,
        );
    }

    pub(crate) fn process_pending_runtime_node_add(&mut self) {
        if self.runtime_node_add_in_progress {
            return;
        }
        let Some((kind, node_name)) = self.pending_runtime_node_add.take() else {
            return;
        };
        self.start_runtime_node_add(kind, &node_name);
    }

    pub(crate) fn poll_runtime_node_add_result(&mut self) {
        let result = self
            .runtime_node_add_rx
            .as_ref()
            .and_then(|rx| rx.try_recv().ok());
        let Some(result) = result else {
            return;
        };

        self.runtime_node_add_rx = None;
        self.runtime_node_add_in_progress = false;
        match result.result {
            Ok(response) if (200..300).contains(&response.status) => {
                self.apply_runtime_node_added(
                    result.kind,
                    &result.node_name,
                    &response,
                    result.label,
                );
            }
            Ok(response) => {
                self.status = format!("Add {} failed: HTTP {}", result.label, response.status);
                self.show_info(
                    "Runtime node",
                    &format!("{} {}", self.status, response.body),
                );
            }
            Err(error) => {
                self.status = format!("Add {} failed: {error}", result.label);
                self.show_info("Runtime node", &self.status.clone());
            }
        }
    }

    fn start_runtime_node_add(&mut self, kind: NodeKind, node_name: &str) {
        if self.runtime_node_add_in_progress {
            return;
        }
        let node_name = node_name.trim();
        let label = runtime_node_kind_label(kind);
        let add_key = format!("{label}:{node_name}");
        let now = Instant::now();
        if self
            .runtime_node_last_add
            .as_ref()
            .is_some_and(|(key, at)| {
                key == &add_key && now.duration_since(*at) < Duration::from_millis(750)
            })
        {
            return;
        }
        self.runtime_node_add_in_progress = true;
        self.runtime_node_last_add = Some((add_key, now));
        if node_name.is_empty() {
            self.status = "Node name is required".to_string();
            self.show_info("Runtime node", "Node name is required");
            self.runtime_node_add_in_progress = false;
            return;
        }

        let (tx, rx) = mpsc::channel();
        self.runtime_node_add_rx = Some(rx);
        let node_name = node_name.to_string();
        thread::spawn(move || {
            let result = add_node_with_daemon_retry(kind, &node_name);
            let _ = tx.send(RuntimeNodeAddResult {
                kind,
                node_name,
                label,
                result,
            });
        });
    }

    fn reload_runtime_node_module_and_add(
        &mut self,
        kind: NodeKind,
        node_name: &str,
    ) -> Option<rtsyn_ui::Result<ApiResponse>> {
        let module_path = self.runtime_module_path_for_kind(kind);
        let module_path = module_path.trim();
        if module_path.is_empty() {
            return None;
        }

        let module = match build_runtime_module(module_path) {
            Ok(module) => module,
            Err(error) => return Some(Err(error)),
        };
        let module_root = module.module_root;
        let module_path_text = module.shared_library.to_string_lossy().to_string();
        match load_node_with_daemon_retry(kind, &module_path_text) {
            Ok(response) if (200..300).contains(&response.status) => {
                if let Some(metadata) = runtime_node_metadata_from_response(&response) {
                    let loaded_name = metadata.name.clone();
                    self.register_runtime_module_metadata(kind, metadata, module_root, None);
                    self.set_runtime_node_name_if_empty_or_inferred(kind, &loaded_name);
                    self.remember_runtime_node_name(kind, &loaded_name);
                    let add_name = if loaded_name == node_name {
                        node_name
                    } else {
                        loaded_name.as_str()
                    };
                    Some(add_node_with_daemon_retry(kind, add_name))
                } else {
                    Some(Err(rtsyn_ui::Error::Api(
                        "module load did not return a descriptor".to_string(),
                    )))
                }
            }
            Ok(response) => Some(Ok(response)),
            Err(error) => Some(Err(error)),
        }
    }

    fn runtime_module_path_for_kind(&self, kind: NodeKind) -> String {
        match kind {
            NodeKind::Plugin => self.windows.load_plugin_path.clone(),
            NodeKind::Device => self.windows.load_device_path.clone(),
        }
    }

    fn apply_runtime_node_added(
        &mut self,
        kind: NodeKind,
        node_name: &str,
        response: &ApiResponse,
        label: &str,
    ) {
        let mut canonical_node_name = node_name.to_string();
        let mut runtime_node_id = None;
        let mut added_metadata = None;
        if let Some(result) = runtime_node_result_from_response(response) {
            runtime_node_id =
                (result.node_id != invalid_runtime_node_id()).then_some(result.node_id);
            let metadata = result.node;
            canonical_node_name = metadata.name.clone();
            added_metadata = Some(metadata.clone());
            self.register_runtime_module_metadata(
                kind,
                metadata,
                PathBuf::new(),
                self.runtime_module_library_path(&canonical_node_name)
                    .map(PathBuf::from),
            );
        }
        let Some(id) = runtime_node_id.map(u64::from) else {
            self.status = format!("{label} node add is pending; no runtime node id was returned");
            self.show_info("Runtime node", &self.status.clone());
            return;
        };
        let default_params = added_metadata
            .as_ref()
            .map(|metadata| {
                runtime_node_default_param_assignments(
                    &self.plugin_manager.installed_plugins,
                    &canonical_node_name,
                    metadata,
                )
            })
            .unwrap_or_default();
        let mut config = serde_json::json!({
            "node_name": canonical_node_name.clone(),
            "api_managed": true,
            "node_type": label,
            "library_path": self.runtime_module_library_path(&canonical_node_name)
        });
        if let Value::Object(ref mut map) = config {
            for (_, name, value, value_type) in &default_params {
                map.insert(name.clone(), runtime_param_json_value(*value_type, value));
            }
            mark_runtime_params_applied(map, &default_params);
        }
        self.workspace_manager.workspace.plugins.push(
            crate::gui::workspace_model::PluginDefinition {
                id,
                kind: canonical_node_name.clone(),
                config,
                priority: 0,
                running: false,
            },
        );
        self.apply_runtime_node_param_defaults(id, &default_params, label);
        self.subscribe_runtime_node_values(id, &canonical_node_name);
        self.remember_runtime_node_name(kind, &canonical_node_name);
        self.status = format!("{label} node added: {canonical_node_name}");
        self.windows.plugins_open = false;
        self.windows.runtime_node_selected_index = None;
        self.invalidate_name_cache();
        self.mark_workspace_dirty();
        self.show_info("Runtime node", &self.status.clone());
    }

    fn apply_runtime_node_param_defaults(
        &mut self,
        node_id: u64,
        params: &[(u32, String, String, ValueType)],
        label: &str,
    ) {
        if params.is_empty() {
            return;
        }
        let Ok(node_id) = u32::try_from(node_id) else {
            self.show_info("Runtime node", "Node id is out of range.");
            return;
        };
        for (param_id, _, value, value_type) in params {
            match ApiClient::default().set_param(node_id, *param_id, *value_type, value) {
                Ok(response) if (200..300).contains(&response.status) => {}
                Ok(response) => {
                    self.status = format!(
                        "Set default {label} param {param_id} failed: HTTP {} {}",
                        response.status, response.body
                    );
                    self.show_info("Runtime node", &self.status.clone());
                    return;
                }
                Err(error) => {
                    self.status = format!("Set default {label} param {param_id} failed: {error}");
                    self.show_info("Runtime node", &self.status.clone());
                    return;
                }
            }
        }
    }

    fn subscribe_runtime_node_values(&mut self, node_id: u64, node_name: &str) {
        let Ok(node_id_u32) = u32::try_from(node_id) else {
            self.show_info("Runtime node", "Node id is out of range.");
            return;
        };
        let Some(installed) = self
            .plugin_manager
            .installed_plugins
            .iter()
            .find(|plugin| plugin.manifest.kind == node_name)
        else {
            return;
        };
        if let Some(schema) = installed.display_schema.as_ref() {
            let port_mask = mask_from_display_entries(&schema.inputs)
                | mask_from_display_entries(&schema.outputs);
            let state_mask = mask_from_display_entries(&schema.variables);
            if port_mask != 0 {
                match ApiClient::default().subscribe_port_values(node_id_u32, true, port_mask) {
                    Ok(response) if (200..300).contains(&response.status) => {}
                    Ok(response) => self.show_info(
                        "Runtime node",
                        &format!("Subscribe ports failed: HTTP {}", response.status),
                    ),
                    Err(error) => {
                        self.show_info("Runtime node", &format!("Subscribe ports failed: {error}"))
                    }
                }
            }
            if state_mask != 0 {
                match ApiClient::default().subscribe_node_states(node_id_u32, true, state_mask) {
                    Ok(response) if (200..300).contains(&response.status) => {}
                    Ok(response) => self.show_info(
                        "Runtime node",
                        &format!("Subscribe states failed: HTTP {}", response.status),
                    ),
                    Err(error) => {
                        self.show_info("Runtime node", &format!("Subscribe states failed: {error}"))
                    }
                }
            }
        }
    }

    pub(crate) fn restore_workspace_runtime_nodes(&mut self) {
        let saved_nodes = self.workspace_manager.workspace.plugins.clone();
        let saved_connections = self.workspace_manager.workspace.connections.clone();
        let mut id_map: HashMap<u64, u64> = HashMap::new();

        for saved_node in saved_nodes {
            let is_api_managed = saved_node
                .config
                .get("api_managed")
                .and_then(|value| value.as_bool())
                .unwrap_or(false);
            let node_kind = match saved_node
                .config
                .get("node_type")
                .and_then(|value| value.as_str())
            {
                Some("device") => NodeKind::Device,
                _ => NodeKind::Plugin,
            };
            let has_loaded_module = self
                .remembered_runtime_module_for_descriptor(node_kind, &saved_node.kind)
                .is_some();
            if !is_api_managed
                && !workspace_node_has_runtime_descriptor(
                    &self.plugin_manager.installed_plugins,
                    &saved_node.kind,
                )
                && !has_loaded_module
            {
                continue;
            }

            if self.runtime_module_library_path(&saved_node.kind).is_none() {
                let module_path = saved_node
                    .config
                    .get("library_path")
                    .and_then(|value| value.as_str())
                    .filter(|value| !value.trim().is_empty())
                    .map(str::to_string)
                    .or_else(|| {
                        self.remembered_runtime_module_for_descriptor(node_kind, &saved_node.kind)
                    });
                if let Some(module_path) = module_path {
                    match node_kind {
                        NodeKind::Plugin => self.load_runtime_plugin_module(&module_path),
                        NodeKind::Device => self.load_runtime_device_module(&module_path),
                    }
                }
            }

            let response = match add_node_with_daemon_retry(node_kind, &saved_node.kind) {
                Ok(response) if (200..300).contains(&response.status) => response,
                Ok(response) => {
                    self.show_info(
                        "Runtime node",
                        &format!(
                            "Restore node {} failed: HTTP {} {}",
                            saved_node.kind, response.status, response.body
                        ),
                    );
                    continue;
                }
                Err(error) => {
                    self.show_info(
                        "Runtime node",
                        &format!("Restore node {} failed: {error}", saved_node.kind),
                    );
                    continue;
                }
            };

            let Some(result) = runtime_node_result_from_response(&response) else {
                continue;
            };
            if result.node_id == invalid_runtime_node_id() {
                continue;
            }

            let new_id = u64::from(result.node_id);
            id_map.insert(saved_node.id, new_id);
            let library_path = self.runtime_module_library_path(&saved_node.kind);
            let default_params = runtime_node_default_param_assignments(
                &self.plugin_manager.installed_plugins,
                &saved_node.kind,
                &result.node,
            );
            let params_to_apply: Vec<(u32, String, String, ValueType)> = default_params
                .iter()
                .map(|(param_id, name, default_value, value_type)| {
                    let value = saved_node
                        .config
                        .get(name)
                        .map(|value| runtime_param_text_value(value, *value_type))
                        .unwrap_or_else(|| default_value.clone());
                    (*param_id, name.clone(), value, *value_type)
                })
                .collect();
            if let Some(node) = self
                .workspace_manager
                .workspace
                .plugins
                .iter_mut()
                .find(|node| node.id == saved_node.id)
            {
                node.id = new_id;
                node.running = false;
                if let serde_json::Value::Object(ref mut map) = node.config {
                    map.insert("api_managed".to_string(), serde_json::Value::from(true));
                    map.insert(
                        "library_path".to_string(),
                        library_path
                            .map(serde_json::Value::from)
                            .unwrap_or(serde_json::Value::Null),
                    );
                    for (_, name, value, value_type) in &params_to_apply {
                        map.insert(name.clone(), runtime_param_json_value(*value_type, value));
                    }
                    mark_runtime_params_applied(map, &params_to_apply);
                }
            }
            self.apply_runtime_node_param_defaults(
                new_id,
                &params_to_apply,
                runtime_node_kind_label(node_kind),
            );
            self.subscribe_runtime_node_values(new_id, &saved_node.kind);
        }

        if !id_map.is_empty() {
            for connection in &mut self.workspace_manager.workspace.connections {
                if let Some(id) = id_map.get(&connection.from_plugin) {
                    connection.from_plugin = *id;
                }
                if let Some(id) = id_map.get(&connection.to_plugin) {
                    connection.to_plugin = *id;
                }
            }
            let connections = self.workspace_manager.workspace.connections.clone();
            for connection in connections {
                let _ = self.push_runtime_add_connection(&connection);
            }
        } else {
            for connection in saved_connections {
                let _ = self.push_runtime_add_connection(&connection);
            }
        }

        self.sync_next_plugin_id();
    }

    pub(crate) fn hydrate_runtime_nodes_from_daemon(&mut self) -> Result<(), String> {
        let response = ApiClient::default()
            .runtime_nodes()
            .map_err(|error| format!("Runtime node snapshot failed: {error}"))?;
        if !(200..300).contains(&response.status) {
            return Err(runtime_nodes_snapshot_http_error(&response));
        }

        let snapshot = serde_json::from_str::<RuntimeNodesSnapshot>(&response.body)
            .map_err(|error| format!("Runtime node snapshot parse failed: {error}"))?;

        self.windows.recent_plugin_names.clear();
        self.windows.recent_device_names.clear();
        for descriptor in &snapshot.loaded_descriptors {
            let kind = node_kind_from_metadata(&descriptor.node);
            self.register_runtime_module_metadata(
                kind,
                descriptor.node.clone(),
                PathBuf::new(),
                self.runtime_module_library_path(&descriptor.node.name)
                    .map(PathBuf::from),
            );
            self.remember_runtime_node_name(kind, &descriptor.node.name);
        }

        self.workspace_manager.workspace.plugins.clear();
        self.workspace_manager.workspace.connections.clear();
        let node_metadata_by_id: HashMap<u32, RuntimeNodeMetadata> = snapshot
            .nodes
            .iter()
            .map(|result| (result.node_id, result.node.clone()))
            .collect();
        let mut restored_nodes = 0usize;
        for result in &snapshot.nodes {
            if result.node_id == invalid_runtime_node_id() {
                continue;
            }

            let kind = node_kind_from_metadata(&result.node);
            let label = runtime_node_kind_label(kind);
            let node_name = result.node.name.clone();
            self.register_runtime_module_metadata(
                kind,
                result.node.clone(),
                PathBuf::new(),
                self.runtime_module_library_path(&node_name)
                    .map(PathBuf::from),
            );

            let id = u64::from(result.node_id);
            self.workspace_manager.workspace.plugins.push(
                crate::gui::workspace_model::PluginDefinition {
                    id,
                    kind: node_name.clone(),
                    config: serde_json::json!({
                        "node_name": node_name.clone(),
                        "api_managed": true,
                        "node_type": label,
                        "library_path": self.runtime_module_library_path(&node_name)
                    }),
                    priority: 0,
                    running: runtime_state_is_running(&result.runtime_state),
                },
            );
            self.subscribe_runtime_node_values(id, &node_name);
            self.remember_runtime_node_name(kind, &node_name);
            restored_nodes += 1;
        }

        for param in snapshot.param_values {
            let Some(node) = self
                .workspace_manager
                .workspace
                .plugins
                .iter_mut()
                .find(|node| node.id == u64::from(param.node_id))
            else {
                continue;
            };
            let Some(metadata) = node_metadata_by_id.get(&param.node_id) else {
                continue;
            };
            let Some(param_metadata) = metadata
                .params
                .iter()
                .find(|item| item.id == param.param_id)
            else {
                continue;
            };
            if let Value::Object(ref mut map) = node.config {
                map.insert(param_metadata.name.clone(), param.value.clone());
                let value_text = match &param.value {
                    Value::String(value) => value.clone(),
                    Value::Number(value) => value.to_string(),
                    Value::Bool(value) => {
                        if *value {
                            "1".to_string()
                        } else {
                            "0".to_string()
                        }
                    }
                    _ => continue,
                };
                mark_runtime_params_applied(
                    map,
                    &[(
                        param.param_id,
                        param_metadata.name.clone(),
                        value_text,
                        ValueType::parse(param_metadata.value_type.as_str())
                            .unwrap_or(ValueType::F64),
                    )],
                );
            }
        }

        let mut restored_connections = 0usize;
        for connection in snapshot.connections {
            let Some(source) = node_metadata_by_id.get(&connection.source_node_id) else {
                continue;
            };
            let Some(destination) = node_metadata_by_id.get(&connection.destination_node_id) else {
                continue;
            };
            let Some(source_port) = source
                .ports
                .iter()
                .find(|port| port.id == connection.source_port_id)
                .map(|port| port.name.clone())
            else {
                continue;
            };
            let Some(destination_port) = destination
                .ports
                .iter()
                .find(|port| port.id == connection.destination_port_id)
                .map(|port| port.name.clone())
            else {
                continue;
            };
            self.workspace_manager.workspace.connections.push(
                crate::gui::workspace_model::ConnectionDefinition {
                    from_plugin: u64::from(connection.source_node_id),
                    from_port: source_port,
                    to_plugin: u64::from(connection.destination_node_id),
                    to_port: destination_port,
                    kind: "same_cycle".to_string(),
                },
            );
            restored_connections += 1;
        }
        if restored_connections > 0 {
            self.connections_view_enabled = true;
        }

        self.state_sync.computed_outputs.clear();
        self.state_sync.input_values.clear();
        self.state_sync.internal_variable_values.clear();
        for value in snapshot.latest_values {
            let Some(metadata) = node_metadata_by_id.get(&value.node_id) else {
                continue;
            };
            if value.kind == "state" {
                let Some(state_name) = metadata
                    .states
                    .iter()
                    .find(|state| state.id == value.value_id)
                    .map(|state| state.name.clone())
                else {
                    continue;
                };
                self.state_sync
                    .internal_variable_values
                    .insert((u64::from(value.node_id), state_name), value.value);
                continue;
            }
            let Some(port) = metadata.ports.iter().find(|port| port.id == value.value_id) else {
                continue;
            };
            let Some(number) = value.value.as_f64() else {
                continue;
            };
            let key = (u64::from(value.node_id), port.name.clone());
            if port.direction == "input" {
                self.state_sync.input_values.insert(key, number);
            } else {
                self.state_sync.computed_outputs.insert(key, number);
            }
        }

        self.sync_next_plugin_id();
        self.invalidate_name_cache();
        self.status = format!(
            "Using existing daemon state: {restored_nodes} node(s), {restored_connections} connection(s) restored"
        );
        Ok(())
    }

    fn remember_runtime_module(&mut self, kind: NodeKind, module_path: &str) {
        let list = match kind {
            NodeKind::Plugin => &mut self.windows.recent_plugin_modules,
            NodeKind::Device => &mut self.windows.recent_device_modules,
        };
        remember_recent(list, module_path);
    }

    fn remember_runtime_node_name(&mut self, kind: NodeKind, node_name: &str) {
        let list = match kind {
            NodeKind::Plugin => &mut self.windows.recent_plugin_names,
            NodeKind::Device => &mut self.windows.recent_device_names,
        };
        remember_recent(list, node_name);
    }

    fn set_runtime_node_name_if_empty_or_inferred(&mut self, kind: NodeKind, node_name: &str) {
        let field = match kind {
            NodeKind::Plugin => &mut self.windows.add_plugin_name,
            NodeKind::Device => &mut self.windows.add_device_name,
        };
        let current = field.trim();
        if current.is_empty() || current == "xmake" {
            *field = node_name.to_string();
        }
    }

    fn runtime_module_library_path(&self, node_name: &str) -> Option<String> {
        self.plugin_manager
            .installed_plugins
            .iter()
            .find(|plugin| plugin.manifest.kind == node_name)
            .and_then(|plugin| plugin.library_path.as_ref())
            .map(|path| path.to_string_lossy().to_string())
    }

    fn remembered_runtime_module_for_descriptor(
        &self,
        kind: NodeKind,
        node_name: &str,
    ) -> Option<String> {
        let modules = match kind {
            NodeKind::Plugin => &self.windows.recent_plugin_modules,
            NodeKind::Device => &self.windows.recent_device_modules,
        };
        modules
            .iter()
            .find(|module| runtime_module_descriptor_name(module) == node_name)
            .cloned()
    }

    fn register_runtime_module_metadata(
        &mut self,
        kind: NodeKind,
        metadata: RuntimeNodeMetadata,
        mut module_root: PathBuf,
        mut shared_library: Option<PathBuf>,
    ) {
        let node_name = metadata.name.clone();
        let existing = self
            .plugin_manager
            .installed_plugins
            .iter()
            .find(|plugin| plugin.manifest.kind == node_name)
            .map(|plugin| {
                (
                    plugin.path.clone(),
                    plugin.library_path.clone(),
                    plugin.ui_schema.is_some(),
                )
        });
        if let Some((existing_path, existing_library, _)) = &existing {
            if module_root.as_os_str().is_empty() && !existing_path.as_os_str().is_empty() {
                module_root = existing_path.clone();
            }
            if shared_library.is_none() {
                shared_library = existing_library.clone();
            }
        }
        if module_root.as_os_str().is_empty() {
            if let Some(module_path) =
                self.remembered_runtime_module_for_descriptor(kind, &node_name)
            {
                if let Ok(root) = runtime_module_root(Path::new(&module_path)) {
                    module_root = root;
                }
            }
        }
        if module_root.as_os_str().is_empty()
            && existing.as_ref().is_some_and(|(path, _, has_ui_schema)| {
                !path.as_os_str().is_empty() || *has_ui_schema
            })
        {
            if kind == NodeKind::Plugin {
                let behavior = self
                    .behavior_manager
                    .cached_behaviors
                    .entry(node_name)
                    .or_default();
                behavior.supports_apply = true;
            }
            return;
        }
        let ui_metadata = load_runtime_node_ui_metadata(&module_root);
        self.plugin_manager
            .installed_plugins
            .retain(|plugin| plugin.manifest.kind != node_name);
        self.plugin_manager
            .installed_plugins
            .push(metadata.into_installed_plugin(module_root, shared_library, ui_metadata));
        if kind == NodeKind::Plugin {
            let behavior = self
                .behavior_manager
                .cached_behaviors
                .entry(node_name)
                .or_default();
            behavior.supports_apply = true;
        }
        self.invalidate_display_schema_cache();
        self.invalidate_name_cache();
    }

    /// Creates a duplicate of an existing plugin in the workspace.
    ///
    /// # Parameters
    /// - `plugin_id`: Unique identifier of the plugin to duplicate
    ///
    /// # Side Effects
    /// - Creates new plugin instance with unique ID
    /// - Caches behavior information for duplicated plugin
    /// - Updates status message
    /// - Marks workspace as dirty
    /// - Shows error notification if plugin ID is invalid
    pub(crate) fn duplicate_plugin(&mut self, plugin_id: u64) {
        let Some(source) = self
            .workspace_manager
            .workspace
            .plugins
            .iter()
            .find(|p| p.id == plugin_id)
            .cloned()
        else {
            self.show_info("Plugin", "Invalid plugin");
            return;
        };

        let is_api_managed = source
            .config
            .get("api_managed")
            .and_then(|value| value.as_bool())
            .unwrap_or(false);
        if is_api_managed {
            let node_kind = match source
                .config
                .get("node_type")
                .and_then(|value| value.as_str())
                .unwrap_or("plugin")
            {
                "device" => NodeKind::Device,
                _ => NodeKind::Plugin,
            };
            let label = runtime_node_kind_label(node_kind);
            match add_node_with_daemon_retry(node_kind, &source.kind) {
                Ok(response) if (200..300).contains(&response.status) => {
                    let Some(result) = runtime_node_result_from_response(&response) else {
                        self.status = format!("Duplicate {label} failed: invalid API response");
                        self.show_info("Plugin", &self.status.clone());
                        return;
                    };
                    if result.node_id == invalid_runtime_node_id() {
                        self.status = format!("Duplicate {label} failed: invalid runtime node id");
                        self.show_info("Plugin", &self.status.clone());
                        return;
                    }

                    let metadata = result.node;
                    let canonical_node_name = metadata.name.clone();
                    self.register_runtime_module_metadata(
                        node_kind,
                        metadata,
                        PathBuf::new(),
                        self.runtime_module_library_path(&canonical_node_name)
                            .map(PathBuf::from),
                    );

                    let id = u64::from(result.node_id);
                    let mut config = source.config.clone();
                    if let Some(map) = config.as_object_mut() {
                        map.insert(
                            "node_name".to_string(),
                            serde_json::Value::from(canonical_node_name.clone()),
                        );
                        map.insert("api_managed".to_string(), serde_json::Value::from(true));
                        map.insert("node_type".to_string(), serde_json::Value::from(label));
                        map.insert(
                            "library_path".to_string(),
                            self.runtime_module_library_path(&canonical_node_name)
                                .map(serde_json::Value::from)
                                .unwrap_or(serde_json::Value::Null),
                        );
                    }
                    self.workspace_manager.workspace.plugins.push(
                        crate::gui::workspace_model::PluginDefinition {
                            id,
                            kind: canonical_node_name.clone(),
                            config,
                            priority: source.priority,
                            running: false,
                        },
                    );
                    self.subscribe_runtime_node_values(id, &canonical_node_name);
                    self.remember_runtime_node_name(node_kind, &canonical_node_name);
                    self.status = format!("{label} node duplicated: {canonical_node_name}");
                    self.invalidate_name_cache();
                    self.mark_workspace_dirty();
                    self.show_info("Runtime node", &self.status.clone());
                }
                Ok(response) => {
                    self.status = format!("Duplicate {label} failed: HTTP {}", response.status);
                    self.show_info("Plugin", &format!("{} {}", self.status, response.body));
                }
                Err(error) => {
                    self.status = format!("Duplicate {label} failed: {error}");
                    self.show_info("Plugin", &self.status.clone());
                }
            }
            return;
        }

        if let Some(source) = self
            .workspace_manager
            .workspace
            .plugins
            .iter()
            .find(|p| p.id == plugin_id)
        {
            let kind = source.kind.clone();
            let library_path = source
                .config
                .get("library_path")
                .and_then(|v| v.as_str())
                .map(std::path::PathBuf::from);
            self.ensure_plugin_behavior_cached_with_path(&kind, library_path.as_ref());
        }
        match self
            .plugin_manager
            .duplicate_plugin_in_workspace(&mut self.workspace_manager.workspace, plugin_id)
        {
            Ok(_) => {}
            Err(_) => {
                self.show_info("Plugin", "Invalid plugin");
                return;
            }
        };
        self.status = "Plugin duplicated".to_string();
        self.mark_workspace_dirty();
    }

    /// Starts every plugin in the current workspace.
    pub(crate) fn start_all_plugins(&mut self) {
        let client = ApiClient::default();
        let metadata_by_kind: HashMap<String, Vec<(String, f64)>> = self
            .plugin_manager
            .installed_plugins
            .iter()
            .map(|plugin| {
                (
                    plugin.manifest.kind.clone(),
                    plugin.metadata_variables.clone(),
                )
            })
            .collect();
        let mut changed = false;
        let mut failures = Vec::new();
        for plugin in &mut self.workspace_manager.workspace.plugins {
            if !plugin.running {
                let Ok(node_id) = u32::try_from(plugin.id) else {
                    failures.push("Node id is out of range.".to_string());
                    continue;
                };
                let params = metadata_by_kind
                    .get(&plugin.kind)
                    .cloned()
                    .unwrap_or_default();
                let mut values = Vec::new();
                for (param_id, (name, default_value)) in params.iter().enumerate() {
                    let value = plugin
                        .config
                        .get(name)
                        .and_then(|value| value.as_f64())
                        .unwrap_or(*default_value);
                    values.push((
                        param_id as u32,
                        name.clone(),
                        format_f64_with_input(&value.to_string(), value),
                        ValueType::F64,
                    ));
                }
                match client.transition_node(node_id, NodeState::Start) {
                    Ok(response) if (200..300).contains(&response.status) => {}
                    Ok(response) => {
                        failures.push(format!(
                            "Start node {} failed: HTTP {}",
                            plugin.id, response.status
                        ));
                        continue;
                    }
                    Err(error) => {
                        failures.push(format!("Start node {} failed: {error}", plugin.id));
                        continue;
                    }
                }

                let mut param_failed = false;
                for (param_id, _, value, value_type) in &values {
                    match client.set_param(node_id, *param_id, *value_type, value) {
                        Ok(response) if (200..300).contains(&response.status) => {}
                        Ok(response) => {
                            failures.push(format!(
                                "Set param {param_id} for node {} failed: HTTP {} {}",
                                plugin.id, response.status, response.body
                            ));
                            param_failed = true;
                            break;
                        }
                        Err(error) => {
                            failures.push(format!(
                                "Set param {param_id} for node {} failed: {error}",
                                plugin.id
                            ));
                            param_failed = true;
                            break;
                        }
                    }
                }
                if !param_failed {
                    if let Value::Object(ref mut map) = plugin.config {
                        mark_runtime_params_applied(map, &values);
                    }
                }
                plugin.running = true;
                changed = true;
            }
        }
        if changed {
            self.open_running_plotters();
            self.mark_workspace_dirty();
        }
        if let Some(failure) = failures.first() {
            self.show_info("Runtime", failure);
        }
    }

    /// Stops every running plugin in the current workspace.
    pub(crate) fn stop_all_plugins(&mut self) {
        let client = ApiClient::default();
        let mut changed = false;
        let mut failures = Vec::new();
        for plugin in &mut self.workspace_manager.workspace.plugins {
            if plugin.running {
                let Ok(node_id) = u32::try_from(plugin.id) else {
                    failures.push("Node id is out of range.".to_string());
                    continue;
                };
                match client.transition_node(node_id, NodeState::Stop) {
                    Ok(response) if (200..300).contains(&response.status) => {}
                    Ok(response) => {
                        failures.push(format!(
                            "Stop node {} failed: HTTP {}",
                            plugin.id, response.status
                        ));
                        continue;
                    }
                    Err(error) => {
                        failures.push(format!("Stop node {} failed: {error}", plugin.id));
                        continue;
                    }
                }
                plugin.running = false;
                changed = true;
            }
        }
        if changed {
            for plotter in self.plotter_manager.plotters.values() {
                if let Ok(mut plotter) = plotter.lock() {
                    plotter.open = false;
                }
            }
            self.recompute_plotter_ui_hz();
            self.mark_workspace_dirty();
        }
        if let Some(failure) = failures.first() {
            self.show_info("Runtime", failure);
        }
    }

    /// Removes a plugin from the workspace by index.
    ///
    /// # Parameters
    /// - `plugin_index`: Index of the plugin in the workspace plugins list
    ///
    /// # Side Effects
    /// - Validates plugin index bounds
    /// - Clears selection if removed plugin was selected
    /// - Closes configuration window if open for removed plugin
    /// - Removes associated plotter data
    /// - Updates extendable input counts for remaining plugins
    /// - Recomputes plotter UI refresh rate
    /// - Enforces connection dependencies
    /// - Updates status message
    /// - Marks workspace as dirty
    pub(crate) fn remove_plugin(&mut self, plugin_index: usize) {
        if plugin_index >= self.workspace_manager.workspace.plugins.len() {
            self.status = "Invalid plugin selection".to_string();
            return;
        }

        let removed_id = self.workspace_manager.workspace.plugins[plugin_index].id;
        let is_api_managed = self.workspace_manager.workspace.plugins[plugin_index]
            .config
            .get("api_managed")
            .and_then(|value| value.as_bool())
            .unwrap_or(false);
        if is_api_managed {
            let Ok(node_id) = u32::try_from(removed_id) else {
                self.status = "Runtime node id is out of range".to_string();
                return;
            };
            match ApiClient::default().remove_node(node_id) {
                Ok(response) if (200..300).contains(&response.status) => {}
                Ok(response) => {
                    self.status = format!("Remove runtime node failed: HTTP {}", response.status);
                    self.show_info("Runtime node", &self.status.clone());
                    return;
                }
                Err(error) => {
                    self.status = format!("Remove runtime node failed: {error}");
                    self.show_info("Runtime node", &self.status.clone());
                    return;
                }
            }
        }

        // Clear highlight if removed plugin was highlighted
        if matches!(self.highlight_mode, HighlightMode::AllConnections(id) if id == removed_id) {
            self.highlight_mode = HighlightMode::None;
        }
        if let HighlightMode::SingleConnection(from, to) = self.highlight_mode {
            if from == removed_id || to == removed_id {
                self.highlight_mode = HighlightMode::None;
            }
        }
        if self.windows.plugin_config_id == Some(removed_id) {
            self.windows.plugin_config_id = None;
            self.windows.plugin_config_open = false;
        }
        self.plotter_manager.plotters.remove(&removed_id);
        self.plotter_manager
            .plotter_preview_settings
            .remove(&removed_id);
        self.plugin_positions.remove(&removed_id);
        self.state_plugin_positions.remove(&removed_id);
        self.plugin_rects.remove(&removed_id);

        if let Err(err) = self
            .plugin_manager
            .remove_plugin_from_workspace(&mut self.workspace_manager.workspace, removed_id)
        {
            self.status = err;
            return;
        }
        let ids: Vec<u64> = self
            .workspace_manager
            .workspace
            .plugins
            .iter()
            .map(|p| p.id)
            .collect();
        for id in ids {
            self.sync_extendable_input_count(id);
        }
        self.recompute_plotter_ui_hz();
        self.enforce_connection_dependent();
        self.status = "Plugin removed".to_string();
        self.mark_workspace_dirty();
    }

    pub(crate) fn remove_plugin_by_id(&mut self, plugin_id: u64) {
        if let Some(index) = self
            .workspace_manager
            .workspace
            .plugins
            .iter()
            .position(|p| p.id == plugin_id)
        {
            self.remove_plugin(index);
        }
    }

    /// Uninstalls a plugin and removes all instances from workspace.
    ///
    /// # Parameters
    /// - `installed_index`: Index of the plugin in the installed plugins list
    ///
    /// # Side Effects
    /// - Uninstalls plugin from system
    /// - Removes all workspace instances of the plugin type
    /// - Clears UI state for removed plugins (selection, config windows, plotters)
    /// - Rescans for detected plugins
    /// - Shows success/error notifications
    pub(crate) fn uninstall_plugin(&mut self, installed_index: usize) {
        let plugin = match self
            .plugin_manager
            .uninstall_plugin_by_index(installed_index)
        {
            Ok(plugin) => plugin,
            Err(err) => {
                self.show_info("Plugin", &err);
                return;
            }
        };
        self.invalidate_display_schema_cache();

        let removed_ids = self.plugin_manager.remove_plugins_by_kind_from_workspace(
            &mut self.workspace_manager.workspace,
            &plugin.manifest.kind,
        );

        for id in &removed_ids {
            // Clear highlight if removed plugin was highlighted
            if matches!(self.highlight_mode, HighlightMode::AllConnections(hid) if hid == *id) {
                self.highlight_mode = HighlightMode::None;
            }
            if let HighlightMode::SingleConnection(from, to) = self.highlight_mode {
                if from == *id || to == *id {
                    self.highlight_mode = HighlightMode::None;
                }
            }
            if self.windows.plugin_config_id == Some(*id) {
                self.windows.plugin_config_id = None;
                self.windows.plugin_config_open = false;
            }
            self.plotter_manager.plotters.remove(id);
            self.plotter_manager.plotter_preview_settings.remove(id);
            self.plugin_positions.remove(id);
            self.state_plugin_positions.remove(id);
            self.plugin_rects.remove(id);
        }

        self.scan_detected_plugins();
        self.invalidate_name_cache();
        self.show_info("Plugin", "Plugin uninstalled");
    }

    /// Installs a plugin from a folder path.
    ///
    /// # Parameters
    /// - `folder`: Path to the plugin folder
    /// - `removable`: Whether the plugin can be uninstalled
    /// - `persist`: Whether to persist the installation
    ///
    /// # Side Effects
    /// - Installs plugin using metadata source for validation
    /// - Updates status message
    /// - Shows error notifications on failure
    /// - Drains compatibility warnings to notifications
    pub(crate) fn install_plugin_from_folder<P: AsRef<Path>>(
        &mut self,
        folder: P,
        removable: bool,
        persist: bool,
    ) {
        let metadata = GuiMetadataSource {
            logic_tx: &self.state_sync.logic_tx,
        };
        if let Err(err) = self.plugin_manager.install_plugin_from_folder(
            folder.as_ref(),
            removable,
            persist,
            &metadata,
        ) {
            self.status = err;
            self.show_info("Plugin Install Error", &self.status.clone());
            return;
        }
        self.invalidate_display_schema_cache();
        self.status = "Plugin installed".to_string();
        self.drain_plugin_compatibility_warnings_to_notifications();
    }

    /// Refreshes an installed plugin with updated code from path.
    ///
    /// # Parameters
    /// - `kind`: Plugin type identifier
    /// - `path`: Path to the updated plugin files
    ///
    /// # Side Effects
    /// - Removes UI state for existing plugin instances
    /// - Refreshes plugin installation if path is not empty
    /// - Updates status message
    /// - Shows error notifications on failure
    /// - Drains compatibility warnings to notifications
    pub(crate) fn refresh_installed_plugin(&mut self, kind: String, path: &Path) {
        let plugin_ids: Vec<u64> = self
            .workspace_manager
            .workspace
            .plugins
            .iter()
            .filter(|p| p.kind == kind)
            .map(|p| p.id)
            .collect();

        for id in &plugin_ids {
            // Clear highlight if refreshed plugin was highlighted
            if matches!(self.highlight_mode, HighlightMode::AllConnections(hid) if hid == *id) {
                self.highlight_mode = HighlightMode::None;
            }
            if let HighlightMode::SingleConnection(from, to) = self.highlight_mode {
                if from == *id || to == *id {
                    self.highlight_mode = HighlightMode::None;
                }
            }
            if self.windows.plugin_config_id == Some(*id) {
                self.windows.plugin_config_id = None;
                self.windows.plugin_config_open = false;
            }
            self.plotter_manager.plotters.remove(id);
            self.plotter_manager.plotter_preview_settings.remove(id);
            self.plugin_positions.remove(id);
            self.state_plugin_positions.remove(id);
            self.plugin_rects.remove(id);
        }

        if path.as_os_str().is_empty() {
            self.status = "Plugin refreshed".to_string();
            return;
        }
        let metadata = GuiMetadataSource {
            logic_tx: &self.state_sync.logic_tx,
        };
        if let Err(err) = self
            .plugin_manager
            .refresh_installed_plugin(&kind, path, &metadata)
        {
            self.status = err;
            self.show_info("Plugin Refresh Error", &self.status.clone());
            return;
        }
        self.invalidate_display_schema_cache();
        self.status = "Plugin refreshed".to_string();
        self.invalidate_name_cache();
        self.drain_plugin_compatibility_warnings_to_notifications();
    }

    /// Refreshes library paths for all installed plugins.
    ///
    /// # Side Effects
    /// Updates the library paths in the plugin manager's installed plugins list
    pub(crate) fn refresh_installed_library_paths(&mut self) {
        self.plugin_manager.refresh_installed_library_paths();
    }

    /// Injects current library paths into workspace plugin definitions.
    ///
    /// # Side Effects
    /// Updates library_path field for all plugins in the current workspace
    pub(crate) fn inject_library_paths_into_workspace(&mut self) {
        self.plugin_manager
            .inject_library_paths_into_workspace(&mut self.workspace_manager.workspace);
    }

    pub(crate) fn normalize_workspace_runtime_node_kinds(&mut self) {
        let aliases = runtime_node_kind_aliases(&self.plugin_manager.installed_plugins);
        if aliases.is_empty() {
            return;
        }

        let mut changed = false;
        for plugin in &mut self.workspace_manager.workspace.plugins {
            let Some(canonical) = aliases.get(&normalized_lookup_key(&plugin.kind)).cloned() else {
                continue;
            };
            if plugin.kind != canonical {
                plugin.kind = canonical.clone();
                changed = true;
            }
            if let serde_json::Value::Object(ref mut map) = plugin.config {
                let needs_node_name = map
                    .get("node_name")
                    .and_then(|value| value.as_str())
                    .map(|node_name| node_name != canonical)
                    .unwrap_or(true);
                if needs_node_name {
                    map.insert("node_name".to_string(), serde_json::Value::from(canonical));
                    changed = true;
                }
            }
        }

        if changed {
            self.invalidate_display_schema_cache();
            self.invalidate_name_cache();
        }
    }

    /// Loads all installed plugins from the plugin directory.
    ///
    /// # Side Effects
    /// - Scans and loads plugin manifests and metadata
    /// - Drains compatibility warnings to notifications
    pub(crate) fn load_installed_plugins(&mut self) {
        self.plugin_manager.load_installed_plugins();
        self.invalidate_display_schema_cache();
        self.drain_plugin_compatibility_warnings_to_notifications();
    }

    /// Refreshes metadata cache for installed plugins with incomplete metadata.
    ///
    /// # Side Effects
    /// - Identifies plugins with missing metadata (inputs, outputs, variables, schemas)
    /// - Queries runtime for updated metadata using metadata source
    /// - Updates plugin manager's cached metadata
    pub(crate) fn refresh_installed_plugin_metadata_cache(&mut self) {
        let targets: Vec<(String, PathBuf)> = self
            .plugin_manager
            .installed_plugins
            .iter()
            .filter(|plugin| {
                if plugin.path.as_os_str().is_empty() {
                    return false;
                }
                plugin.metadata_inputs.is_empty()
                    || plugin.metadata_outputs.is_empty()
                    || plugin.metadata_variables.is_empty()
                    || plugin.display_schema.is_none()
            })
            .map(|plugin| (plugin.manifest.kind.clone(), plugin.path.clone()))
            .collect();
        if targets.is_empty() {
            return;
        }

        let metadata = GuiMetadataSource {
            logic_tx: &self.state_sync.logic_tx,
        };
        for (kind, path) in targets {
            let _ = self
                .plugin_manager
                .refresh_installed_plugin(&kind, &path, &metadata);
        }
    }
}

fn load_node_with_daemon_retry(
    kind: NodeKind,
    module_path: &str,
) -> rtsyn_ui::Result<rtsyn_ui::api::ApiResponse> {
    let client = ApiClient::default();
    let response = client.load_node(kind, module_path)?;
    if response.status != 404 {
        return Ok(response);
    }

    restart_default_daemon()?;
    ApiClient::default().load_node(kind, module_path)
}

fn add_node_with_daemon_retry(
    kind: NodeKind,
    node_name: &str,
) -> rtsyn_ui::Result<rtsyn_ui::api::ApiResponse> {
    let client = ApiClient::default();
    let response = client.add_node(kind, node_name)?;
    if response.status != 404 {
        return Ok(response);
    }

    restart_default_daemon()?;
    ApiClient::default().add_node(kind, node_name)
}

fn restart_default_daemon() -> rtsyn_ui::Result<()> {
    let daemon = DaemonController::default_for_api(rtsyn_ui::DEFAULT_API_BASE_URL);
    daemon.stop()?;
    daemon.start()?;
    Ok(())
}

impl RuntimeNodeMetadata {
    fn into_installed_plugin(
        self,
        module_root: PathBuf,
        shared_library: Option<PathBuf>,
        ui_metadata: Option<PluginUiMetadata>,
    ) -> InstalledPlugin {
        let inputs: Vec<String> = self
            .ports
            .iter()
            .filter(|port| port.direction == "input" || port.direction == "in")
            .map(|port| port.name.clone())
            .collect();
        let outputs: Vec<String> = self
            .ports
            .iter()
            .filter(|port| port.direction == "output" || port.direction == "out")
            .map(|port| port.name.clone())
            .collect();
        let display_inputs: Vec<String> = self
            .ports
            .iter()
            .filter(|port| port.direction == "input" || port.direction == "in")
            .map(|port| format!("{}|{}", port.id, port.name))
            .collect();
        let display_outputs: Vec<String> = self
            .ports
            .iter()
            .filter(|port| port.direction == "output" || port.direction == "out")
            .map(|port| format!("{}|{}", port.id, port.name))
            .collect();
        let display_states: Vec<String> = self
            .states
            .iter()
            .map(|state| format!("{}|{}", state.id, state.name))
            .collect();
        let params: Vec<(String, f64)> = self
            .params
            .iter()
            .filter_map(|param| {
                let value_type =
                    ValueType::parse(param.value_type.as_str()).unwrap_or(ValueType::F64);
                if !matches!(value_type, ValueType::F32 | ValueType::F64) {
                    return None;
                }
                let default = ui_metadata
                    .as_ref()
                    .and_then(|metadata| {
                        metadata
                            .controls
                            .iter()
                            .find(|control| control.name == param.name)
                    })
                    .map(|control| control.default_value.as_str())
                    .unwrap_or(param.default.as_str());
                Some((
                    param.name.clone(),
                    default.trim().parse::<f64>().unwrap_or(0.0),
                ))
            })
            .collect();
        let ui_schema = runtime_node_ui_schema(&self.params, ui_metadata.as_ref());
        let manifest_name = ui_metadata
            .as_ref()
            .map(|metadata| metadata.name.trim())
            .filter(|name| !name.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| self.name.clone());
        let description = ui_metadata
            .as_ref()
            .map(|metadata| metadata.description.trim())
            .filter(|description| !description.is_empty())
            .map(str::to_string)
            .or_else(|| {
                if self.description.trim().is_empty() {
                    None
                } else {
                    Some(self.description)
                }
            });

        InstalledPlugin {
            manifest: PluginManifest {
                name: manifest_name,
                kind: self.name,
                version: Some("1.0.0".to_string()),
                description,
                library: shared_library.as_ref().and_then(|shared_library| {
                    shared_library
                        .file_name()
                        .and_then(|name| name.to_str())
                        .map(|name| name.to_string())
                }),
                api_version: None,
            },
            path: module_root,
            library_path: shared_library,
            removable: false,
            metadata_inputs: inputs.clone(),
            metadata_outputs: outputs.clone(),
            metadata_variables: params,
            display_schema: Some(DisplaySchema {
                inputs: display_inputs,
                outputs: display_outputs,
                variables: display_states,
            }),
            ui_schema,
        }
    }
}

fn runtime_node_ui_schema(
    params: &[RuntimeNodeParamMetadata],
    ui_metadata: Option<&PluginUiMetadata>,
) -> Option<UISchema> {
    if let Some(metadata) = ui_metadata {
        let fields: Vec<UIField> = metadata
            .controls
            .iter()
            .filter_map(|control| runtime_ui_field_from_control(control, params))
            .collect();
        if !fields.is_empty() {
            return Some(UISchema { fields });
        }
    }

    let fields: Vec<UIField> = params
        .iter()
        .map(|param| {
            let value_type = ValueType::parse(param.value_type.as_str()).unwrap_or(ValueType::F64);
            let default = if param.default.is_empty() {
                default_runtime_param_value(value_type)
            } else {
                runtime_param_json_value(value_type, &param.default)
            };
            let mut field = UIField::new(
                param.name.clone(),
                runtime_field_type(param.name.as_str(), value_type),
            )
            .label(param.name.clone())
            .description(param.description.clone())
            .default(default);
            field.value_type = Some(value_type.name().to_string());
            field
        })
        .collect();

    if fields.is_empty() {
        None
    } else {
        Some(UISchema { fields })
    }
}

fn runtime_ui_field_from_control(
    control: &PluginControl,
    params: &[RuntimeNodeParamMetadata],
) -> Option<UIField> {
    let param = control
        .param_id
        .and_then(|param_id| params.iter().find(|param| param.id == param_id))
        .or_else(|| params.iter().find(|param| param.name == control.name))?;
    let default = if control.default_value.is_empty() {
        default_runtime_param_value(control.value_type)
    } else {
        runtime_param_json_value(control.value_type, control.default_value.as_str())
    };
    let mut field = UIField::new(
        control.name.clone(),
        runtime_control_field_type(control, param),
    )
    .label(control.label.clone())
    .description(param.description.clone())
    .default(default);
    field.value_type = Some(control.value_type.name().to_string());
    Some(field)
}

fn runtime_control_field_type(
    control: &PluginControl,
    param: &RuntimeNodeParamMetadata,
) -> FieldType {
    match control.kind {
        ControlKind::Toggle => FieldType::Boolean,
        ControlKind::Text if is_path_param(control.name.as_str(), param.name.as_str()) => {
            FieldType::FilePath { placeholder: None }
        }
        ControlKind::Text => FieldType::Text { placeholder: None },
        ControlKind::Number => runtime_field_type(param.name.as_str(), control.value_type),
    }
}

fn runtime_field_type(name: &str, value_type: ValueType) -> FieldType {
    match value_type {
        ValueType::I64 | ValueType::U64 => FieldType::Integer {
            min: None,
            max: None,
            step: 1,
        },
        ValueType::F32 | ValueType::F64 => FieldType::Float {
            min: None,
            max: None,
            step: 0.1,
        },
        ValueType::String if is_path_param(name, name) => FieldType::FilePath { placeholder: None },
        ValueType::String => FieldType::Text { placeholder: None },
    }
}

fn is_path_param(control_name: &str, param_name: &str) -> bool {
    control_name.ends_with("_path")
        || control_name == "path"
        || param_name.ends_with("_path")
        || param_name == "path"
}

fn default_runtime_param_value(value_type: ValueType) -> Value {
    match value_type {
        ValueType::String => Value::String(String::new()),
        ValueType::I64 | ValueType::U64 => Value::from(0),
        ValueType::F32 | ValueType::F64 => Value::from(0.0),
    }
}

fn runtime_node_metadata_from_response(response: &ApiResponse) -> Option<RuntimeNodeMetadata> {
    runtime_node_result_from_response(response).map(|result| result.node)
}

fn runtime_node_result_from_response(response: &ApiResponse) -> Option<RuntimeCommandResult> {
    serde_json::from_str::<RuntimeCommandResult>(&response.body).ok()
}

fn load_runtime_node_ui_metadata(module_root: &Path) -> Option<PluginUiMetadata> {
    PluginUiMetadata::read_from(&module_root.join("rtsyn-node-ui.toml")).ok()
}

fn response_is_unknown_node(response: &ApiResponse) -> bool {
    response.status == 503
        && serde_json::from_str::<RuntimeCommandStatus>(&response.body)
            .map(|status| status.status_code == 1)
            .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::{
        canonical_runtime_node_kind_for, mask_from_display_entries, queue_runtime_node_add_request,
        response_is_unknown_node, runtime_module_descriptor_name,
        runtime_node_default_param_assignments, runtime_node_metadata_from_response,
        runtime_node_result_from_response, runtime_nodes_snapshot_http_error,
        workspace_node_has_runtime_descriptor,
    };
    use crate::gui::tool_model::plugin::{InstalledPlugin, PluginManifest};
    use rtsyn_ui::api::{ApiResponse, NodeKind};
    use rtsyn_ui::metadata::PluginUiMetadata;
    use rtsyn_ui::module::runtime_module_root;
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique_temp_dir(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("rtsyn-gui-{name}-{nanos}"))
    }

    #[test]
    fn runtime_node_add_queue_accepts_only_one_pending_add() {
        let mut pending = None;

        assert!(queue_runtime_node_add_request(
            &mut pending,
            false,
            NodeKind::Plugin,
            " adder "
        ));
        assert!(!queue_runtime_node_add_request(
            &mut pending,
            false,
            NodeKind::Plugin,
            "adder"
        ));
        assert_eq!(pending, Some((NodeKind::Plugin, "adder".to_string())));

        pending = None;
        assert!(!queue_runtime_node_add_request(
            &mut pending,
            true,
            NodeKind::Plugin,
            "adder"
        ));
        assert!(pending.is_none());
    }

    #[test]
    fn runtime_module_root_accepts_project_directory() {
        let root = unique_temp_dir("project-dir");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("xmake.lua"), "").unwrap();

        assert_eq!(
            runtime_module_root(&root).unwrap(),
            root.canonicalize().unwrap()
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn runtime_module_root_accepts_xmake_file() {
        let root = unique_temp_dir("xmake-file");
        fs::create_dir_all(&root).unwrap();
        let xmake = root.join("xmake.lua");
        fs::write(&xmake, "").unwrap();

        assert_eq!(
            runtime_module_root(&xmake).unwrap(),
            root.canonicalize().unwrap()
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn runtime_nodes_snapshot_404_reports_incompatible_daemon() {
        let response = ApiResponse {
            status: 404,
            body: String::new(),
        };

        assert_eq!(
            runtime_nodes_snapshot_http_error(&response),
            "Running daemon does not expose runtime node snapshots. Restart daemon to preserve state."
        );
    }

    #[test]
    fn runtime_node_metadata_from_api_result_preserves_descriptor_fields() {
        let response = ApiResponse {
            status: 202,
            body: r#"{"seq":1,"command_type":"load_node","success":true,"status_code":0,"node_id":4294967295,"node":{"name":"adder","node_type":"plugin","ports":[{"id":0,"name":"left","direction":"input","value_type":"f64"},{"id":2,"name":"sum","direction":"output","value_type":"f64"}],"params":[{"id":0,"name":"left_multiplier","description":"Multiplier","value_type":"f64"}],"states":[{"id":0,"name":"result","description":"Result","value_type":"f64"}]}}"#.to_string(),
        };

        let metadata = runtime_node_metadata_from_response(&response).unwrap();
        let installed = metadata.into_installed_plugin(
            PathBuf::from("/tmp/rtsyn-adder"),
            Some(PathBuf::from("/tmp/rtsyn-adder/librtsyn-adder.so")),
            None,
        );

        assert_eq!(installed.manifest.kind, "adder");
        assert_eq!(installed.manifest.name, "adder");
        assert_eq!(installed.metadata_inputs, vec!["left"]);
        assert_eq!(installed.metadata_outputs, vec!["sum"]);
        assert_eq!(
            installed.metadata_variables,
            vec![("left_multiplier".to_string(), 0.0)]
        );
        let display_schema = installed.display_schema.unwrap();
        assert_eq!(display_schema.inputs, vec!["0|left".to_string()]);
        assert_eq!(display_schema.outputs, vec!["2|sum".to_string()]);
        assert_eq!(display_schema.variables, vec!["0|result".to_string()]);
    }

    #[test]
    fn canonical_runtime_node_kind_accepts_descriptor_name_and_display_name() {
        let installed = vec![InstalledPlugin {
            manifest: PluginManifest {
                name: "Forwarder".to_string(),
                kind: "forwarder".to_string(),
                version: None,
                description: None,
                library: None,
                api_version: None,
            },
            path: PathBuf::new(),
            library_path: None,
            removable: false,
            metadata_inputs: Vec::new(),
            metadata_outputs: Vec::new(),
            metadata_variables: Vec::new(),
            display_schema: None,
            ui_schema: None,
        }];

        assert_eq!(
            canonical_runtime_node_kind_for(&installed, "forwarder").as_deref(),
            Some("forwarder")
        );
        assert_eq!(
            canonical_runtime_node_kind_for(&installed, "Forwarder").as_deref(),
            Some("forwarder")
        );
    }

    #[test]
    fn workspace_node_has_runtime_descriptor_accepts_loaded_display_name() {
        let installed = vec![InstalledPlugin {
            manifest: PluginManifest {
                name: "Adder".to_string(),
                kind: "adder".to_string(),
                version: None,
                description: None,
                library: None,
                api_version: None,
            },
            path: PathBuf::new(),
            library_path: None,
            removable: false,
            metadata_inputs: Vec::new(),
            metadata_outputs: Vec::new(),
            metadata_variables: Vec::new(),
            display_schema: None,
            ui_schema: None,
        }];

        assert!(workspace_node_has_runtime_descriptor(&installed, "Adder"));
        assert!(workspace_node_has_runtime_descriptor(&installed, "adder"));
        assert!(!workspace_node_has_runtime_descriptor(
            &installed, "missing"
        ));
    }

    #[test]
    fn runtime_node_result_preserves_runtime_node_id() {
        let response = ApiResponse {
            status: 202,
            body: r#"{"seq":2,"command_type":"add_node","success":true,"status_code":0,"node_id":17,"node":{"name":"forwarder","node_type":"plugin","ports":[],"params":[],"states":[]}}"#.to_string(),
        };

        let result = runtime_node_result_from_response(&response).unwrap();

        assert_eq!(result.node_id, 17);
        assert_eq!(result.node.name, "forwarder");
    }

    #[test]
    fn runtime_module_descriptor_name_accepts_built_library_and_xmake_root() {
        assert_eq!(
            runtime_module_descriptor_name(
                "/home/seregio/Desktop/stuff/projects/rtsyn/rtsyn-forwarder/build/linux/x86_64/release/librtsyn-forwarder.so"
            ),
            "forwarder"
        );
        assert_eq!(
            runtime_module_descriptor_name(
                "/home/seregio/Desktop/stuff/projects/rtsyn/rtsyn-adder/xmake.lua"
            ),
            "adder"
        );
    }

    #[test]
    fn mask_from_display_entries_uses_descriptor_ids() {
        let mask = mask_from_display_entries(&[
            "0|left".to_string(),
            "2|sum".to_string(),
            "ignored".to_string(),
            "64|outside-mask".to_string(),
        ]);

        assert_eq!(mask, (1_u64 << 0) | (1_u64 << 2));
    }

    #[test]
    fn runtime_node_metadata_uses_toml_name_and_defaults_when_available() {
        let response = ApiResponse {
            status: 202,
            body: r#"{"seq":1,"command_type":"load_node","success":true,"status_code":0,"node_id":4294967295,"node":{"name":"adder","node_type":"plugin","ports":[],"params":[{"id":0,"name":"left_multiplier","description":"Multiplier","value_type":"f64"}],"states":[]}}"#.to_string(),
        };
        let ui_metadata = PluginUiMetadata::parse(
            r#"
name = "Adder"
description = "Adds two values."

[[controls]]
name = "left_multiplier"
label = "Left multiplier"
kind = "number"
target = "param"
param_id = 0
value_type = "f64"
default = "1.0"
"#,
        )
        .unwrap();

        let installed = runtime_node_metadata_from_response(&response)
            .unwrap()
            .into_installed_plugin(PathBuf::new(), None, Some(ui_metadata));

        assert_eq!(installed.manifest.kind, "adder");
        assert_eq!(installed.manifest.name, "Adder");
        assert_eq!(
            installed.manifest.description.as_deref(),
            Some("Adds two values.")
        );
        assert_eq!(
            installed.metadata_variables,
            vec![("left_multiplier".to_string(), 1.0)]
        );
    }

    #[test]
    fn runtime_node_default_param_assignments_use_ui_metadata_defaults() {
        let response = ApiResponse {
            status: 202,
            body: r#"{"seq":1,"command_type":"load_node","success":true,"status_code":0,"node_id":4294967295,"node":{"name":"rthybrid_hindmarsh_rose_1984_neuron_v2","node_type":"plugin","ports":[],"params":[{"id":0,"name":"x0","description":"Initial x state","value_type":"f64"},{"id":1,"name":"e","description":"Injected current","value_type":"f64"},{"id":2,"name":"burst_duration_s","description":"Burst duration","value_type":"f64"}],"states":[]}}"#.to_string(),
        };
        let metadata = runtime_node_metadata_from_response(&response).unwrap();
        let ui_metadata = PluginUiMetadata::parse(
            r#"
name = "Hindmarsh-Rose"
description = "Hindmarsh-Rose neuron."

[[controls]]
name = "x0"
label = "x0"
kind = "number"
target = "param"
param_id = 0
value_type = "f64"
default = "-0.9013747551021072"

[[controls]]
name = "e"
label = "e"
kind = "number"
target = "param"
param_id = 1
value_type = "f64"
default = "3.0"

[[controls]]
name = "burst_duration_s"
label = "Burst duration"
kind = "number"
target = "param"
param_id = 2
value_type = "f64"
default = "1.0"
"#,
        )
        .unwrap();
        let installed =
            metadata
                .clone()
                .into_installed_plugin(PathBuf::new(), None, Some(ui_metadata));

        let assignments = runtime_node_default_param_assignments(
            &[installed],
            "rthybrid_hindmarsh_rose_1984_neuron_v2",
            &metadata,
        );

        assert_eq!(
            assignments,
            vec![
                (
                    0,
                    "x0".to_string(),
                    "-0.9013747551021072".to_string(),
                    rtsyn_ui::api::ValueType::F64,
                ),
                (
                    1,
                    "e".to_string(),
                    "3".to_string(),
                    rtsyn_ui::api::ValueType::F64,
                ),
                (
                    2,
                    "burst_duration_s".to_string(),
                    "1".to_string(),
                    rtsyn_ui::api::ValueType::F64,
                ),
            ]
        );
    }

    #[test]
    fn runtime_node_metadata_maps_string_path_control_to_file_path_field() {
        let response = ApiResponse {
            status: 202,
            body: r#"{"seq":1,"command_type":"load_node","success":true,"status_code":0,"node_id":4294967295,"node":{"name":"comedi","node_type":"device","ports":[],"params":[{"id":0,"name":"device_path","description":"COMEDI device path","value_type":"string"}],"states":[]}}"#.to_string(),
        };
        let ui_metadata = PluginUiMetadata::parse(
            r#"
name = "COMEDI"
description = "COMEDI device."

[[controls]]
name = "device_path"
label = "Device path"
kind = "text"
target = "param"
param_id = 0
value_type = "string"
default = "/dev/comedi0"
"#,
        )
        .unwrap();

        let installed = runtime_node_metadata_from_response(&response)
            .unwrap()
            .into_installed_plugin(PathBuf::new(), None, Some(ui_metadata));

        assert!(installed.metadata_variables.is_empty());
        let schema = installed.ui_schema.unwrap();
        assert_eq!(schema.fields.len(), 1);
        assert_eq!(schema.fields[0].key, "device_path");
        assert_eq!(schema.fields[0].value_type.as_deref(), Some("string"));
        assert!(matches!(
            schema.fields[0].field_type,
            crate::gui::tool_api::ui::FieldType::FilePath { .. }
        ));
        assert_eq!(
            schema.fields[0]
                .default
                .as_ref()
                .and_then(|value| value.as_str()),
            Some("/dev/comedi0")
        );
    }

    #[test]
    fn runtime_node_metadata_keeps_forwarder_param_out_of_states() {
        let response = ApiResponse {
            status: 202,
            body: r#"{"seq":1,"command_type":"load_node","success":true,"status_code":0,"node_id":4294967295,"node":{"name":"forwarder","node_type":"plugin","ports":[{"id":0,"name":"out","direction":"output","value_type":"f64"}],"params":[{"id":0,"name":"value","description":"Value","value_type":"f64"}],"states":[]}}"#.to_string(),
        };

        let installed = runtime_node_metadata_from_response(&response)
            .unwrap()
            .into_installed_plugin(PathBuf::new(), None, None);
        let display_schema = installed.display_schema.unwrap();

        assert_eq!(installed.metadata_outputs, vec!["out"]);
        assert_eq!(
            installed.metadata_variables,
            vec![("value".to_string(), 0.0)]
        );
        assert_eq!(display_schema.outputs, vec!["0|out"]);
        assert!(display_schema.variables.is_empty());
    }

    #[test]
    fn response_is_unknown_node_matches_engine_status_code() {
        let response = ApiResponse {
            status: 503,
            body: r#"{"seq":2,"command_type":"add_node","success":false,"status_code":1,"node_id":4294967295,"node":{"name":"","node_type":"plugin","ports":[],"params":[],"states":[]}}"#.to_string(),
        };

        assert!(response_is_unknown_node(&response));

        let response = ApiResponse {
            status: 503,
            body: r#"{"status_code":7}"#.to_string(),
        };
        assert!(!response_is_unknown_node(&response));
    }
}
