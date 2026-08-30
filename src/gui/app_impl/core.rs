use crate::gui::managers::{
    FileDialogManager, NotificationHandler, PlotterManager, PluginBehaviorManager,
};
use crate::gui::runtime_bridge::{LogicMessage, LogicState};
use crate::gui::state;
use crate::gui::state::{ConnectionEditorHost, FrequencyUnit, PeriodUnit, StateSync, ViewMode};
use crate::gui::tool_model::plugin::PluginManager;
use crate::gui::tool_model::workspace::WorkspaceManager;
use crate::gui::HighlightMode;
use crate::gui::NewPluginDraft;
use crate::gui::{GuiApp, ParsedDisplaySchema};
use crate::gui::workspace_model::DEFAULT_THREAD_PRIORITY;
use eframe::egui::{self};
use rtsyn_ui::daemon::DaemonController;
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender};
use std::time::Instant;

#[derive(Debug, Default, Deserialize, Serialize)]
struct GuiSession {
    #[serde(default)]
    plugin_modules: Vec<String>,
    #[serde(default)]
    device_modules: Vec<String>,
}

impl GuiApp {
    pub(crate) fn new_with_runtime_and_daemon(
        logic_tx: Sender<LogicMessage>,
        logic_state_rx: Receiver<LogicState>,
        daemon: Option<DaemonController>,
        daemon_owned: bool,
        daemon_restart_prompt_open: bool,
    ) -> Self {
        let install_db_path = PathBuf::from("app_plugins").join("installed_plugins.json");
        let workspace_dir = PathBuf::from("app_workspaces");

        let mut plugin_manager = PluginManager::new(install_db_path);
        let mut workspace_manager = WorkspaceManager::new(workspace_dir);
        let file_dialogs = FileDialogManager::new();
        let plotter_manager = PlotterManager::new();
        let state_sync = StateSync::new(logic_tx, logic_state_rx);
        let notification_handler = NotificationHandler::new();
        let behavior_manager = PluginBehaviorManager::new();
        let runtime_plotter_settings = state::PlotterPreviewState {
            title: "Runtime telemetry".to_string(),
            dark_theme: true,
            x_axis_name: "time (ms)".to_string(),
            y_axis_name: "value".to_string(),
            window_ms: 30_000.0,
            refresh_hz: 30.0,
            ..Default::default()
        };

        plugin_manager.refresh_library_paths();
        workspace_manager
            .workspace
            .plugins
            .iter_mut()
            .for_each(|p| {
                if let Some(installed) = plugin_manager
                    .installed_plugins
                    .iter()
                    .find(|i| i.manifest.kind == p.kind)
                {
                    if let Some(lib_path) = &installed.library_path {
                        if let Some(config) = p.config.as_object_mut() {
                            config.insert(
                                "library_path".to_string(),
                                serde_json::Value::String(lib_path.to_string_lossy().to_string()),
                            );
                        }
                    }
                }
            });

        let mut app = Self {
            plugin_manager,
            workspace_manager,
            file_dialogs,
            plotter_manager,
            state_sync,
            notification_handler,
            behavior_manager,
            daemon,
            daemon_owned,
            daemon_restart_prompt_open,
            daemon_exit_prompt_open: false,
            daemon_exit_allow_close: false,
            daemon_status_text: String::new(),
            plotter_preview: state::PlotterPreviewState::default(),
            connection_editor: state::ConnectionEditorState::default(),
            workspace_dialog: state::WorkspaceDialogState::default(),
            build_dialog: state::BuildDialogState::default(),
            pending_build_queue: std::collections::VecDeque::new(),
            confirm_dialog: state::ConfirmDialogState::default(),
            workspace_settings: state::WorkspaceSettingsState::default(),
            help_state: state::HelpState::default(),
            windows: state::WindowState::default(),
            measurements_open: false,
            measurements_text: String::new(),
            last_measurements_update: Instant::now(),
            csv_telemetry_open: false,
            csv_telemetry_path: Self::default_csv_path(),
            csv_telemetry_columns: Vec::new(),
            csv_telemetry_selected_source: None,
            csv_telemetry_selected_value: None,
            csv_telemetry_new_column_name: String::new(),
            csv_telemetry_status: String::new(),
            csv_telemetry_writing: false,
            csv_telemetry_path_dialog_rx: None,
            runtime_plotter_config_open: false,
            runtime_plotter_window_open: false,
            runtime_plotter: crate::gui::plotter::LivePlotter::new(u64::MAX - 1),
            runtime_plotter_series: Vec::new(),
            runtime_plotter_selected_source: None,
            runtime_plotter_selected_value: None,
            runtime_plotter_new_series_name: String::new(),
            runtime_plotter_status: String::new(),
            runtime_plotter_last_update: Instant::now(),
            runtime_plotter_tick: 0,
            runtime_plotter_latest_values: Vec::new(),
            runtime_plotter_settings,
            runtime_plotter_export_rx: None,
            status: String::new(),
            runtime_node_add_in_progress: false,
            runtime_node_last_add: None,
            pending_runtime_node_add: None,
            runtime_node_add_rx: None,
            runtime_param_path_target: None,
            csv_path_target_plugin_id: None,
            plugin_creator_last_path: None,
            new_plugin_draft: NewPluginDraft::default(),
            seen_compatibility_warnings: HashSet::new(),
            plugin_positions: HashMap::new(),
            state_plugin_positions: HashMap::new(),
            plugin_rects: HashMap::new(),
            connections_view_enabled: true,
            connection_clicked_this_frame: false,
            frequency_value: 1000.0,
            frequency_unit: FrequencyUnit::Hz,
            period_value: 1.0,
            period_unit: PeriodUnit::Ms,
            thread_priority: DEFAULT_THREAD_PRIORITY,
            deadline_tolerance_ns: 0,
            output_refresh_hz: 1.0,
            plotter_screenshot_target: None,
            connection_highlight_plugin_id: None,
            highlight_mode: HighlightMode::None,
            pending_highlight: None,
            plugin_context_menu: None,
            connection_context_menu: None,
            connection_editor_host: ConnectionEditorHost::Main,
            number_edit_buffers: HashMap::new(),
            window_rects: Vec::new(),
            pending_window_focus: None,
            uml_preview_texture: None,
            uml_preview_hash: None,
            uml_preview_error: None,
            uml_preview_loading: false,
            uml_preview_rx: None,
            uml_text_buffer: String::new(),
            uml_export_svg: false,
            uml_export_width: 1920,
            uml_export_height: 1080,
            uml_preview_zoom: 0.0,
            view_mode: ViewMode::default(),
            pending_plugin_sections_open: None,
            pending_plugin_order: None,
            plugin_name_cache: None,
            display_schema_cache: RefCell::new(HashMap::new()),
        };
        app.restore_gui_session(!daemon_restart_prompt_open);
        for warning in app.plugin_manager.take_compatibility_warnings() {
            app.show_info("Plugin Compatibility", &warning);
            app.seen_compatibility_warnings.insert(warning);
        }
        app.refresh_installed_plugin_metadata_cache();
        app.apply_workspace_settings();
        app
    }

    fn gui_session_path(&self) -> PathBuf {
        self.workspace_manager
            .workspace_dir()
            .join("gui_session.json")
    }

    fn load_gui_session(&self) -> Option<GuiSession> {
        let data = std::fs::read(self.gui_session_path()).ok()?;
        serde_json::from_slice(&data).ok()
    }

    pub(crate) fn persist_gui_session(&self) {
        let path = self.gui_session_path();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let session = GuiSession {
            plugin_modules: self.windows.recent_plugin_modules.clone(),
            device_modules: self.windows.recent_device_modules.clone(),
        };
        if let Ok(data) = serde_json::to_vec_pretty(&session) {
            let _ = std::fs::write(path, data);
        }
    }

    fn restore_gui_session(&mut self, load_runtime_modules: bool) {
        let Some(session) = self.load_gui_session() else {
            self.scan_workspaces();
            return;
        };

        for module in session.plugin_modules.iter().rev() {
            remember_session_module(&mut self.windows.recent_plugin_modules, module);
            if load_runtime_modules {
                self.load_runtime_plugin_module(module);
            }
        }
        for module in session.device_modules.iter().rev() {
            remember_session_module(&mut self.windows.recent_device_modules, module);
            if load_runtime_modules {
                self.load_runtime_device_module(module);
            }
        }

        self.scan_workspaces();
        self.persist_gui_session();
    }

    pub(crate) fn parsed_display_schema_for_kind(&self, kind: &str) -> ParsedDisplaySchema {
        if let Some(cached) = self.display_schema_cache.borrow().get(kind) {
            return cached.clone();
        }
        let parsed = self
            .plugin_manager
            .installed_plugins
            .iter()
            .find(|plugin| plugin.manifest.kind == kind)
            .map(|plugin| ParsedDisplaySchema::from_installed(plugin))
            .unwrap_or_default();
        self.display_schema_cache
            .borrow_mut()
            .insert(kind.to_string(), parsed.clone());
        parsed
    }

    pub(crate) fn invalidate_display_schema_cache(&self) {
        self.display_schema_cache.borrow_mut().clear();
    }

    /// Handles double-click on a plugin - highlights all its connections.
    /// Handles double-click on a plugin - highlights all its connections.
    /// If plugin is already highlighted, clears highlights.
    pub(crate) fn double_click_plugin(&mut self, plugin_id: u64) {
        // Check if plugin has connections
        let has_connections = self
            .workspace_manager
            .workspace
            .connections
            .iter()
            .any(|c| c.from_plugin == plugin_id || c.to_plugin == plugin_id);

        if !has_connections {
            // Non-connected plugin - just clear any existing highlight
            self.highlight_mode = HighlightMode::None;
            self.pending_highlight = None;
            return;
        }

        // Toggle off only if clicking the SAME plugin again
        if matches!(self.highlight_mode, HighlightMode::AllConnections(id) if id == plugin_id) {
            self.highlight_mode = HighlightMode::None;
            self.pending_highlight = None;
            return;
        }

        // If currently highlighted, clear and set pending for next frame
        if !matches!(self.highlight_mode, HighlightMode::None) {
            self.pending_highlight = Some(HighlightMode::AllConnections(plugin_id));
            self.highlight_mode = HighlightMode::None;
        } else {
            // Direct switch from None
            self.highlight_mode = HighlightMode::AllConnections(plugin_id);
        }
    }

    /// Handles click on a connection - highlights only the two connected plugins.
    pub(crate) fn click_connection(&mut self, from_plugin: u64, to_plugin: u64) {
        // Mark that connection was clicked this frame
        self.connection_clicked_this_frame = true;

        // If clicking the reverse direction of current highlight, keep it (don't switch)
        if matches!(self.highlight_mode, HighlightMode::SingleConnection(f, t)
            if (f == from_plugin && t == to_plugin) || (f == to_plugin && t == from_plugin))
        {
            return;
        }

        // Direct switch (same as double-click behavior)
        self.highlight_mode = HighlightMode::SingleConnection(from_plugin, to_plugin);
    }

    /// Checks if a connection should be highlighted.
    pub(crate) fn should_highlight_connection(&self, from_plugin: u64, to_plugin: u64) -> bool {
        match self.highlight_mode {
            HighlightMode::AllConnections(plugin_id) => {
                from_plugin == plugin_id || to_plugin == plugin_id
            }
            HighlightMode::SingleConnection(from, to) => {
                // Highlight all connections between these two plugins (bidirectional)
                (from_plugin == from && to_plugin == to) || (from_plugin == to && to_plugin == from)
            }
            HighlightMode::None => false,
        }
    }

    /// Gets the set of plugins that should be highlighted based on current highlight mode.
    pub(crate) fn get_highlighted_plugins(&self) -> HashSet<u64> {
        match self.highlight_mode {
            HighlightMode::AllConnections(plugin_id) => {
                let mut set = HashSet::new();
                set.insert(plugin_id);
                for conn in &self.workspace_manager.workspace.connections {
                    if conn.from_plugin == plugin_id || conn.to_plugin == plugin_id {
                        set.insert(conn.from_plugin);
                        set.insert(conn.to_plugin);
                    }
                }
                set
            }
            HighlightMode::SingleConnection(from, to) => {
                let mut set = HashSet::new();
                set.insert(from);
                set.insert(to);
                set
            }
            HighlightMode::None => HashSet::new(),
        }
    }

    /// ```
    pub(crate) fn center_window(ctx: &egui::Context, size: egui::Vec2) -> egui::Pos2 {
        let rect = ctx.available_rect();
        let center = rect.center();
        center - size * 0.5
    }

    /// - Error messages and notifications
    pub(crate) fn display_kind(kind: &str) -> String {
        PluginManager::display_kind(kind)
    }

    pub(crate) fn display_connection_kind(kind: &str) -> &str {
        match kind {
            "shared_memory" => "Shared memory",
            "pipe" => "Pipe",
            "in_process" => "In process",
            "value" => "Value",
            other => other,
        }
    }

    /// - **Cross-platform**: Works on Unix-like systems and Windows
    pub(crate) fn default_csv_path() -> String {
        let base = std::env::var("HOME")
            .map(|home| PathBuf::from(home).join("rtsyn-recorded"))
            .unwrap_or_else(|_| PathBuf::from("rtsyn-recorded"));
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let day = now / 86_400;
        let hour = (now % 86_400) / 3_600;
        let minute = (now % 3_600) / 60;
        let second = now % 60;
        let stamp = format!("{day}-{hour:02}-{minute:02}-{second:02}");
        base.join(format!("{stamp}.csv"))
            .to_string_lossy()
            .to_string()
    }

    pub(crate) fn get_name_by_kind(&mut self) -> std::collections::HashMap<String, String> {
        if self.plugin_name_cache.is_none() {
            self.plugin_name_cache = Some(
                self.plugin_manager
                    .installed_plugins
                    .iter()
                    .map(|p| (p.manifest.kind.clone(), p.manifest.name.clone()))
                    .collect(),
            );
        }
        self.plugin_name_cache.as_ref().unwrap().clone()
    }

    pub(crate) fn invalidate_name_cache(&mut self) {
        self.plugin_name_cache = None;
    }
}

fn remember_session_module(list: &mut Vec<String>, value: &str) {
    let value = value.trim();
    if value.is_empty() {
        return;
    }
    list.retain(|entry| entry != value);
    list.insert(0, value.to_string());
    list.truncate(12);
}
