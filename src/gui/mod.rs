use crate::gui::runtime_bridge::spawn_runtime;
use crate::gui::runtime_bridge::{LogicMessage, LogicState, RuntimeTelemetrySample};
use crate::gui::workspace_model::ConnectionDefinition;
use eframe::{egui, egui::RichText};
use rtsyn_ui::daemon::{DaemonController, DaemonStatus};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::process;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum HighlightMode {
    None,
    AllConnections(u64),
    SingleConnection(u64, u64),
}

// GuiApp implementation modules
mod app_impl;

// Core modules
mod builtin_tools;
mod daemon;
pub mod daemon_bridge;
mod managers;
mod plotter;
pub mod runtime_bridge;
mod state;
mod tool_api;
mod tool_model;
mod ui;
mod utils;
mod workspace_model;

use crate::gui::daemon_bridge::plugin_creator::PluginKindType;
use crate::gui::tool_model::plugin::{InstalledPlugin, PluginManager};
use crate::gui::tool_model::workspace::WorkspaceManager;
use managers::{FileDialogManager, NotificationHandler, PlotterManager, PluginBehaviorManager};
use plotter::LivePlotter;
use rtsyn_ui::api::NodeKind;
use state::{
    ConfirmAction, ConnectionEditorHost, FrequencyUnit, PeriodUnit, PluginOrderMode,
    RuntimeNodeDialogMode, RuntimeNodeDialogTarget, StateSync, ViewMode, WorkspaceDialogMode,
    WorkspaceTimingTab,
};
use utils::{
    distance_to_segment, has_rt_capabilities, kdialog_available, kdialog_file_dialog,
    save_file_dialog, spawn_file_dialog_thread, zenity_available, zenity_file_dialog,
    zenity_file_dialog_with_name, zenity_folder_dialog_multi,
};

const DEDICATED_PLOTTER_VIEW_KINDS: &[&str] = &["live_plotter"];

#[derive(Clone)]
pub(crate) struct DisplayEntry {
    pub(crate) key: String,
    pub(crate) label: String,
}

impl DisplayEntry {
    fn from_raw(entry: &str) -> Self {
        if let Some((key, label)) = entry.split_once('|') {
            Self {
                key: key.trim().to_string(),
                label: label.trim().to_string(),
            }
        } else {
            let trimmed = entry.trim();
            Self {
                key: trimmed.to_string(),
                label: trimmed.to_string(),
            }
        }
    }

    fn from_key(key: &str) -> Self {
        let trimmed = key.trim();
        Self {
            key: trimmed.to_string(),
            label: trimmed.to_string(),
        }
    }
}

#[derive(Clone, Default)]
pub(crate) struct ParsedDisplaySchema {
    pub(crate) inputs: Vec<DisplayEntry>,
    pub(crate) outputs: Vec<DisplayEntry>,
    pub(crate) variables: Vec<DisplayEntry>,
}

impl ParsedDisplaySchema {
    fn from_installed(plugin: &InstalledPlugin) -> Self {
        let schema = plugin.display_schema.as_ref();
        Self {
            inputs: Self::entries(
                schema.map(|schema| schema.inputs.as_slice()),
                plugin.metadata_inputs.iter().map(String::as_str),
            ),
            outputs: Self::entries(
                schema.map(|schema| schema.outputs.as_slice()),
                plugin.metadata_outputs.iter().map(String::as_str),
            ),
            variables: Self::entries(
                schema.map(|schema| schema.variables.as_slice()),
                std::iter::empty::<&str>(),
            ),
        }
    }

    fn entries<'a>(
        primary: Option<&[String]>,
        fallback: impl Iterator<Item = &'a str>,
    ) -> Vec<DisplayEntry> {
        if let Some(list) = primary {
            if !list.is_empty() {
                return list
                    .iter()
                    .map(|entry| DisplayEntry::from_raw(entry))
                    .collect();
            }
        }
        fallback.map(DisplayEntry::from_key).collect()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct CsvTelemetryColumn {
    name: String,
    source: CsvTelemetryColumnSource,
    label: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum CsvTelemetryColumnSource {
    Value {
        node_id: u64,
        value_id: u32,
        kind: CsvTelemetryValueKind,
    },
    Measurement(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum CsvTelemetryValueKind {
    Port,
    State,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct CsvTelemetrySourceOption {
    id: String,
    label: String,
    values: Vec<CsvTelemetryValueOption>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct CsvTelemetryValueOption {
    key: String,
    label: String,
    default_column_name: String,
    source: CsvTelemetryColumnSource,
}

struct RuntimeNodeAddResult {
    kind: NodeKind,
    node_name: String,
    label: &'static str,
    result: rtsyn_ui::Result<rtsyn_ui::api::ApiResponse>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gui::tool_api::ui::DisplaySchema;
    use crate::gui::tool_model::plugin::PluginManifest;

    #[test]
    fn parsed_display_schema_does_not_render_params_as_states() {
        let plugin = InstalledPlugin {
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
            metadata_outputs: vec!["out".to_string()],
            metadata_variables: vec![("value".to_string(), 1.0)],
            display_schema: Some(DisplaySchema {
                inputs: Vec::new(),
                outputs: vec!["out".to_string()],
                variables: Vec::new(),
            }),
            ui_schema: None,
        };

        let parsed = ParsedDisplaySchema::from_installed(&plugin);

        assert_eq!(parsed.outputs.len(), 1);
        assert_eq!(parsed.outputs[0].key, "out");
        assert!(parsed.variables.is_empty());
    }

    #[test]
    fn builds_csv_telemetry_request_columns() {
        let columns = vec![
            CsvTelemetryColumn {
                name: "left".to_string(),
                source: CsvTelemetryColumnSource::Value {
                    node_id: 2,
                    value_id: 4,
                    kind: CsvTelemetryValueKind::Port,
                },
                label: "adder output left".to_string(),
            },
            CsvTelemetryColumn {
                name: "latency".to_string(),
                source: CsvTelemetryColumnSource::Measurement("latency_ns".to_string()),
                label: "Measurements latency".to_string(),
            },
        ];

        let (names, values, measurement_fields) =
            GuiApp::csv_telemetry_request_columns(&columns).unwrap();

        assert_eq!(names, vec!["left", "latency"]);
        assert_eq!(values.len(), 1);
        assert_eq!(values[0].node_id, 2);
        assert_eq!(values[0].value_id, 4);
        assert_eq!(values[0].kind, rtsyn_ui::api::CsvValueKind::Port);
        assert_eq!(measurement_fields, vec!["latency_ns"]);
        assert!(GuiApp::csv_telemetry_request_columns(&[]).is_err());
        assert!(GuiApp::csv_telemetry_request_columns(&[CsvTelemetryColumn {
            name: " ".to_string(),
            source: CsvTelemetryColumnSource::Value {
                node_id: 2,
                value_id: 1,
                kind: CsvTelemetryValueKind::Port,
            },
            label: "empty".to_string(),
        }])
        .is_err());
    }

    #[test]
    fn runtime_plotter_sample_matching_uses_source_identity() {
        let port_series = CsvTelemetryColumn {
            name: "port".to_string(),
            source: CsvTelemetryColumnSource::Value {
                node_id: 7,
                value_id: 2,
                kind: CsvTelemetryValueKind::Port,
            },
            label: "node output".to_string(),
        };
        let state_series = CsvTelemetryColumn {
            name: "state".to_string(),
            source: CsvTelemetryColumnSource::Value {
                node_id: 7,
                value_id: 2,
                kind: CsvTelemetryValueKind::State,
            },
            label: "node state".to_string(),
        };
        let sample = RuntimeTelemetrySample {
            node_id: 7,
            value_id: 2,
            kind: "port".to_string(),
            cycle_id: 11,
            timestamp_ns: 12,
            value: 1.0,
        };

        assert!(GuiApp::runtime_plotter_series_matches_sample(
            &port_series,
            &sample
        ));
        assert!(!GuiApp::runtime_plotter_series_matches_sample(
            &state_series,
            &sample
        ));
        assert!(!GuiApp::runtime_plotter_series_matches_sample(
            &CsvTelemetryColumn {
                name: "other".to_string(),
                source: CsvTelemetryColumnSource::Value {
                    node_id: 8,
                    value_id: 2,
                    kind: CsvTelemetryValueKind::Port,
                },
                label: "other".to_string(),
            },
            &sample
        ));
    }

    #[test]
    fn daemon_launch_action_prompts_only_for_running_daemon() {
        assert_eq!(
            GuiApp::daemon_launch_action(DaemonStatus::Stopped),
            GuiDaemonLaunchAction::StartOwned
        );
        assert_eq!(
            GuiApp::daemon_launch_action(DaemonStatus::Running),
            GuiDaemonLaunchAction::PromptRestart
        );
    }
}

#[derive(Debug, Clone)]
pub struct GuiConfig {
    pub title: String,
    pub width: f32,
    pub height: f32,
}

impl Default for GuiConfig {
    fn default() -> Self {
        Self {
            title: "RTSyn".to_string(),
            width: 1280.0,
            height: 720.0,
        }
    }
}

#[derive(thiserror::Error, Debug)]
pub enum GuiError {
    #[error("gui error: {0}")]
    Gui(String),
}

#[derive(Debug, Clone)]
enum BuildAction {
    Install {
        path: PathBuf,
        removable: bool,
        persist: bool,
    },
    Reinstall {
        kind: String,
        path: PathBuf,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WindowFocus {
    WorkspaceDialog,
    LoadWorkspaces,
    ManageWorkspaces,
    ManagePlugins,
    InstallPlugins,
    UninstallPlugins,
    Plugins,
    NewPlugin,
    WorkspaceSettings,
    UmlDiagram,
    ManageConnections,
    ConnectionEditorAdd,
    ConnectionEditorRemove,
    PluginConfig,
    Help,
}

static NEXT_PLUGIN_FIELD_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone)]
struct PluginFieldDraft {
    id: u64,
    name: String,
    type_name: String,
    default_value: String,
}

impl PluginFieldDraft {
    fn next_id() -> u64 {
        NEXT_PLUGIN_FIELD_ID.fetch_add(1, Ordering::Relaxed)
    }
}

impl Default for PluginFieldDraft {
    fn default() -> Self {
        Self {
            id: PluginFieldDraft::next_id(),
            name: String::new(),
            type_name: "f64".to_string(),
            default_value: "0.0".to_string(),
        }
    }
}

#[derive(Debug, Clone)]
struct NewPluginDraft {
    name: String,
    language: String,
    plugin_type: PluginKindType,
    main_characteristics: String,
    autostart: bool,
    supports_start_stop: bool,
    supports_restart: bool,
    supports_apply: bool,
    external_window: bool,
    starts_expanded: bool,
    required_inputs_all: bool,
    required_outputs_all: bool,
    required_inputs: Vec<String>,
    required_outputs: Vec<String>,
    required_input_selection: String,
    required_output_selection: String,
    variables: Vec<PluginFieldDraft>,
    inputs: Vec<PluginFieldDraft>,
    outputs: Vec<PluginFieldDraft>,
    internal_variables: Vec<PluginFieldDraft>,
}

impl Default for NewPluginDraft {
    fn default() -> Self {
        Self {
            name: String::new(),
            language: "rust".to_string(),
            plugin_type: PluginKindType::Standard,
            main_characteristics: String::new(),
            autostart: false,
            supports_start_stop: true,
            supports_restart: true,
            supports_apply: false,
            external_window: false,
            starts_expanded: true,
            required_inputs_all: false,
            required_outputs_all: false,
            required_inputs: Vec::new(),
            required_outputs: Vec::new(),
            required_input_selection: String::new(),
            required_output_selection: String::new(),
            variables: Vec::new(),
            inputs: Vec::new(),
            outputs: Vec::new(),
            internal_variables: Vec::new(),
        }
    }
}

#[derive(Debug)]
pub(crate) struct BuildResult {
    success: bool,
    action: BuildAction,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct WorkspaceSettingsDraft {
    frequency_value: f64,
    frequency_unit: FrequencyUnit,
    period_value: f64,
    period_unit: PeriodUnit,
    tab: WorkspaceTimingTab,
    thread_priority: i32,
    deadline_tolerance_ns: u64,
}

/// Initializes and runs the RTSyn GUI application with automatic runtime spawning.
///
/// This is the main entry point for the RTSyn GUI application. It handles two
/// execution modes: daemon plugin viewer mode and normal application mode.
/// In normal mode, it spawns the logic runtime and initializes the GUI.
///
/// # Parameters
///
/// * `config` - GUI configuration specifying window title, dimensions, and other settings
///
/// # Returns
///
/// * `Ok(())` - GUI application completed successfully
/// * `Err(GuiError)` - GUI initialization or runtime error occurred
///
/// # Execution Modes
///
/// ## Daemon Plugin Viewer Mode
/// Activated when environment variables are set:
/// - `RTSYN_DAEMON_VIEW_PLUGIN_ID` - Plugin ID to view through the API adapter
///
/// In this mode, the GUI connects to an existing daemon process to view a specific
/// plugin's interface rather than running a full application instance.
///
/// ## Normal Application Mode
/// 1. Spawns the logic runtime using `spawn_runtime()`
/// 2. Creates communication channels between GUI and runtime
/// 3. Delegates to `run_gui_with_runtime()` for GUI initialization
///
/// # Error Handling
///
/// - Runtime spawn failures cause immediate process termination with error message
/// - GUI initialization errors are propagated as `GuiError::Gui`
/// - Environment variable parsing errors fall back to normal mode
///
/// # Side Effects
///
/// - May spawn background runtime threads
/// - Creates GUI window and event loop
/// - In viewer mode, requests state through the API adapter
/// - On runtime failure, prints error and calls `process::exit(1)`
pub fn run_gui(config: GuiConfig) -> Result<(), GuiError> {
    if let Ok(id_str) = std::env::var("RTSYN_DAEMON_VIEW_PLUGIN_ID") {
        if let Ok(plugin_id) = id_str.parse::<u64>() {
            return daemon::run_daemon_plugin_viewer(config, plugin_id);
        }
    }
    let daemon = DaemonController::default_for_api(rtsyn_ui::DEFAULT_API_BASE_URL);
    let daemon_action = GuiApp::daemon_launch_action(daemon.status());
    let mut daemon_owned = false;
    let daemon_restart_prompt_open = match daemon_action {
        GuiDaemonLaunchAction::StartOwned => {
            daemon
                .start()
                .map_err(|error| GuiError::Gui(format!("failed to start RTSyn daemon: {error}")))?;
            daemon_owned = true;
            false
        }
        GuiDaemonLaunchAction::PromptRestart => true,
    };
    let (logic_tx, logic_state_rx) = match spawn_runtime() {
        Ok(tuple) => tuple,
        Err(err) => {
            if daemon_owned {
                let _ = daemon.stop();
            }
            eprintln!("Failed to start logic runtime: {err}");
            process::exit(1);
        }
    };
    run_gui_with_runtime_daemon(
        config,
        logic_tx,
        logic_state_rx,
        Some(daemon),
        daemon_owned,
        daemon_restart_prompt_open,
    )
}

/// Runs the RTSyn GUI application with pre-existing runtime communication channels.
///
/// This function initializes and runs the eframe-based GUI application using provided
/// communication channels to an already-running logic runtime. It configures the
/// GUI framework, sets up fonts, and creates the main application instance.
///
/// # Parameters
///
/// * `config` - GUI configuration containing window title, dimensions, and display settings
/// * `logic_tx` - Sender channel for sending messages to the logic runtime
/// * `logic_state_rx` - Receiver channel for receiving state updates from the logic runtime
///
/// # Returns
///
/// * `Ok(())` - GUI application completed successfully (user closed window)
/// * `Err(GuiError::Gui)` - eframe initialization or runtime error occurred
///
/// # GUI Framework Setup
///
/// 1. **Window Configuration**: Creates native window with specified dimensions
/// 2. **VSync Disabled**: Prevents hangs and lag on occluded windows
/// 3. **Font Setup**: Loads FontAwesome icons for UI elements
/// 4. **Application Creation**: Instantiates `GuiApp` with runtime channels
///
/// # Font Configuration
///
/// Embeds and configures FontAwesome solid icons (fa-solid-900.ttf) for use in
/// buttons and UI elements. The font is added to the proportional font family
/// to enable icon rendering alongside text.
///
/// # Application Lifecycle
///
/// - Creates eframe native options with custom viewport settings
/// - Disables VSync to prevent performance issues with window occlusion
/// - Sets up font definitions including embedded FontAwesome icons
/// - Instantiates GuiApp with runtime communication channels
/// - Runs the event loop until application termination
///
/// # Error Propagation
///
/// eframe errors are wrapped in `GuiError::Gui` and propagated to the caller.
/// The error message includes the original eframe error description.
pub fn run_gui_with_runtime(
    config: GuiConfig,
    logic_tx: Sender<LogicMessage>,
    logic_state_rx: Receiver<LogicState>,
) -> Result<(), GuiError> {
    run_gui_with_runtime_daemon(config, logic_tx, logic_state_rx, None, false, false)
}

fn run_gui_with_runtime_daemon(
    config: GuiConfig,
    logic_tx: Sender<LogicMessage>,
    logic_state_rx: Receiver<LogicState>,
    daemon: Option<DaemonController>,
    daemon_owned: bool,
    daemon_restart_prompt_open: bool,
) -> Result<(), GuiError> {
    let mut options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([config.width, config.height])
            .with_maximized(true),
        ..Default::default()
    };
    // NOTE: Vsync generates hangs and lag on occluded windows.
    options.vsync = false;

    eframe::run_native(
        &config.title,
        options,
        Box::new(move |cc| {
            let mut fonts = egui::FontDefinitions::default();
            fonts.font_data.insert(
                "fa".to_string(),
                egui::FontData::from_static(include_bytes!("../assets/fonts/fa-solid-900.ttf")),
            );
            let family = fonts
                .families
                .entry(egui::FontFamily::Proportional)
                .or_default();
            if !family.contains(&"fa".to_string()) {
                family.push("fa".to_string());
            }
            cc.egui_ctx.set_fonts(fonts);
            Box::new(GuiApp::new_with_runtime_and_daemon(
                logic_tx,
                logic_state_rx,
                daemon,
                daemon_owned,
                daemon_restart_prompt_open,
            ))
        }),
    )
    .map_err(|err| GuiError::Gui(err.to_string()))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GuiDaemonLaunchAction {
    StartOwned,
    PromptRestart,
}

struct GuiApp {
    // Managers
    plugin_manager: PluginManager,
    workspace_manager: WorkspaceManager,
    file_dialogs: FileDialogManager,
    plotter_manager: PlotterManager,
    state_sync: StateSync,
    notification_handler: NotificationHandler,
    behavior_manager: PluginBehaviorManager,
    daemon: Option<DaemonController>,
    daemon_owned: bool,
    daemon_restart_prompt_open: bool,
    daemon_exit_prompt_open: bool,
    daemon_exit_allow_close: bool,
    daemon_status_text: String,

    // UI State Groups
    plotter_preview: state::PlotterPreviewState,
    connection_editor: state::ConnectionEditorState,
    workspace_dialog: state::WorkspaceDialogState,
    build_dialog: state::BuildDialogState,
    pending_build_queue: VecDeque<(BuildAction, String)>,
    confirm_dialog: state::ConfirmDialogState,
    workspace_settings: state::WorkspaceSettingsState,
    help_state: state::HelpState,
    windows: state::WindowState,
    measurements_open: bool,
    measurements_text: String,
    last_measurements_update: Instant,
    csv_telemetry_open: bool,
    csv_telemetry_path: String,
    csv_telemetry_columns: Vec<CsvTelemetryColumn>,
    csv_telemetry_selected_source: Option<String>,
    csv_telemetry_selected_value: Option<String>,
    csv_telemetry_new_column_name: String,
    csv_telemetry_status: String,
    csv_telemetry_writing: bool,
    csv_telemetry_path_dialog_rx: Option<Receiver<Option<PathBuf>>>,
    runtime_plotter_config_open: bool,
    runtime_plotter_window_open: bool,
    runtime_plotter: LivePlotter,
    runtime_plotter_series: Vec<CsvTelemetryColumn>,
    runtime_plotter_selected_source: Option<String>,
    runtime_plotter_selected_value: Option<String>,
    runtime_plotter_new_series_name: String,
    runtime_plotter_status: String,
    runtime_plotter_last_update: Instant,
    runtime_plotter_tick: u64,
    runtime_plotter_latest_values: Vec<f64>,
    runtime_plotter_settings: state::PlotterPreviewState,
    runtime_plotter_export_rx: Option<Receiver<Option<PathBuf>>>,

    // Remaining UI State
    status: String,
    runtime_node_add_in_progress: bool,
    runtime_node_last_add: Option<(String, Instant)>,
    pending_runtime_node_add: Option<(NodeKind, String)>,
    runtime_node_add_rx: Option<Receiver<RuntimeNodeAddResult>>,
    runtime_param_path_target: Option<(u64, String)>,
    csv_path_target_plugin_id: Option<u64>,
    plugin_creator_last_path: Option<PathBuf>,
    new_plugin_draft: NewPluginDraft,
    seen_compatibility_warnings: HashSet<String>,
    plugin_positions: HashMap<u64, egui::Pos2>,
    state_plugin_positions: HashMap<u64, egui::Pos2>,
    plugin_rects: HashMap<u64, egui::Rect>,
    connections_view_enabled: bool,
    connection_clicked_this_frame: bool,
    frequency_value: f64,
    frequency_unit: FrequencyUnit,
    period_value: f64,
    period_unit: PeriodUnit,
    thread_priority: i32,
    deadline_tolerance_ns: u64,
    output_refresh_hz: f64,
    plotter_screenshot_target: Option<u64>,
    connection_highlight_plugin_id: Option<u64>,
    highlight_mode: HighlightMode,
    pending_highlight: Option<HighlightMode>,
    plugin_context_menu: Option<(u64, egui::Pos2, u64)>,
    connection_context_menu: Option<(Vec<ConnectionDefinition>, egui::Pos2, u64)>,
    connection_editor_host: ConnectionEditorHost,
    number_edit_buffers: HashMap<(u64, String), String>,
    window_rects: Vec<egui::Rect>,
    pending_window_focus: Option<WindowFocus>,
    uml_preview_texture: Option<egui::TextureHandle>,
    uml_preview_hash: Option<u64>,
    uml_preview_error: Option<String>,
    uml_preview_loading: bool,
    uml_preview_rx: Option<Receiver<(u64, Result<Vec<u8>, String>)>>,
    uml_text_buffer: String,
    uml_export_svg: bool,
    uml_export_width: u32,
    uml_export_height: u32,
    uml_preview_zoom: f32,
    view_mode: ViewMode,
    pending_plugin_sections_open: Option<bool>,
    pending_plugin_order: Option<PluginOrderMode>,
    plugin_name_cache: Option<std::collections::HashMap<String, String>>,
    display_schema_cache: RefCell<HashMap<String, ParsedDisplaySchema>>,
}

impl GuiApp {
    fn daemon_launch_action(status: DaemonStatus) -> GuiDaemonLaunchAction {
        match status {
            DaemonStatus::Stopped => GuiDaemonLaunchAction::StartOwned,
            DaemonStatus::Running => GuiDaemonLaunchAction::PromptRestart,
        }
    }

    fn plugin_creator_field_names(fields: &[PluginFieldDraft]) -> Vec<String> {
        fields
            .iter()
            .map(|entry| entry.name.trim().to_string())
            .filter(|name| !name.is_empty())
            .collect()
    }
}

impl Drop for GuiApp {
    fn drop(&mut self) {
        if self.daemon_owned {
            if let Some(daemon) = &self.daemon {
                let _ = daemon.stop();
            }
        }
    }
}

impl eframe::App for GuiApp {
    /// Main GUI update loop called by eframe for each frame.
    ///
    /// This method implements the core GUI update cycle, handling user input,
    /// processing runtime state updates, managing UI components, and rendering
    /// the complete application interface.
    ///
    /// # Parameters
    ///
    /// * `ctx` - egui context providing input handling and rendering capabilities
    /// * `_frame` - eframe frame reference (unused in current implementation)
    ///
    /// # Update Cycle Overview
    ///
    /// ## 1. Style Configuration
    /// - Disables selectable labels to prevent unwanted text selection
    /// - Configures UI interaction behavior
    ///
    /// ## 2. Dialog Polling
    /// - Polls all asynchronous file dialogs for completion
    /// - Handles build, install, import, load, export operations
    /// - Processes CSV path selection and plugin creation dialogs
    /// - Updates plotter screenshot operations
    ///
    /// ## 3. Runtime State Processing
    /// - Polls logic runtime for state updates via `poll_logic_state()`
    /// - Updates plotter displays with new data
    /// - Synchronizes GUI state with runtime state
    ///
    /// ## 4. Refresh Rate Management
    /// - Calculates optimal refresh rate based on active plotters
    /// - Requests appropriate repaint timing from egui
    /// - Balances responsiveness with performance
    ///
    /// ## 5. Workspace Synchronization
    /// - Sends workspace updates to runtime when dirty flag is set
    /// - Ensures runtime has current workspace configuration
    /// - Clears dirty flag after successful synchronization
    ///
    /// ## 6. Input Handling
    /// - Processes Escape key for dialog dismissal
    /// - Handles global keyboard shortcuts
    /// - Manages dialog state transitions
    ///
    /// ## 7. UI Rendering
    /// - Renders top menu bar with workspace, plugin, and runtime menus
    /// - Displays main central panel with plugin cards and connections
    /// - Shows all active dialogs and windows
    /// - Handles context menus and popup interactions
    ///
    /// # Refresh Rate Strategy
    ///
    /// ## Active Plotter Mode
    /// - Uses maximum refresh rate from open plotters (minimum 1 Hz)
    /// - Ensures smooth real-time data visualization
    ///
    /// ## Idle Mode
    /// - Uses 250ms refresh interval when window is not focused
    /// - Reduces CPU usage when application is in background
    ///
    /// # Dialog Management
    ///
    /// Renders all possible dialogs and windows:
    /// - Workspace management dialogs
    /// - Plugin installation and configuration windows
    /// - Connection editor and management interfaces
    /// - Plotter preview and configuration dialogs
    /// - Help and information displays
    /// - Confirmation and notification overlays
    ///
    /// # State Management
    ///
    /// - Maintains window rectangle tracking for layout management
    /// - Handles pending window focus requests
    /// - Manages plugin selection and context menu state
    /// - Coordinates between different UI components
    ///
    /// # Performance Considerations
    ///
    /// - Non-blocking runtime communication prevents GUI freezing
    /// - Adaptive refresh rates optimize CPU usage
    /// - Efficient dialog polling minimizes overhead
    /// - Smart repaint requests reduce unnecessary rendering
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if ctx.input(|i| i.viewport().close_requested())
            && self.daemon.is_some()
            && !self.daemon_exit_allow_close
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.daemon_exit_prompt_open = true;
        }

        ctx.style_mut(|style| {
            style.interaction.selectable_labels = false;
        });
        self.poll_build_dialog();
        self.poll_install_dialog();
        self.poll_import_dialog();
        self.poll_load_dialog();
        self.poll_runtime_module_dialog();
        self.poll_runtime_param_path_dialog();
        self.poll_export_dialog();
        self.poll_csv_path_dialog();
        self.poll_csv_telemetry_path_dialog();
        self.poll_plugin_creator_dialog();
        self.poll_plotter_screenshot_dialog();
        self.poll_logic_state();
        self.poll_runtime_node_add_result();
        self.process_pending_runtime_node_add();
        let mut plotter_refresh = 0.0;
        for plotter in self.plotter_manager.plotters.values() {
            if let Ok(plotter) = plotter.lock() {
                if plotter.open && plotter.refresh_hz > plotter_refresh {
                    plotter_refresh = plotter.refresh_hz;
                }
            }
        }
        if plotter_refresh > 0.0 {
            let hz = plotter_refresh.max(1.0);
            ctx.request_repaint_after(Duration::from_secs_f64(1.0 / hz));
        } else if !ctx.input(|i| i.focused) {
            ctx.request_repaint_after(Duration::from_millis(250));
        }
        if self.workspace_manager.workspace_dirty {
            let _ = self.state_sync.logic_tx.send(LogicMessage::UpdateWorkspace(
                self.workspace_manager.workspace.clone(),
            ));
            self.workspace_manager.workspace_dirty = false;
        }
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            if self.build_dialog.open && !self.build_dialog.in_progress {
                self.build_dialog.open = false;
            } else if self.confirm_dialog.open {
                self.confirm_dialog.open = false;
                self.confirm_dialog.action = None;
            }
        }
        self.window_rects.clear();

        egui::TopBottomPanel::top("top_bar").show(ctx, |ui| {
            ui.scope(|ui| {
                let mut style = ui.style().as_ref().clone();
                style.spacing.button_padding = egui::vec2(10.0, 6.0);
                style
                    .text_styles
                    .insert(egui::TextStyle::Button, egui::FontId::proportional(15.0));
                ui.set_style(style);
                egui::menu::bar(ui, |ui| {
                    ui.menu_button("Workspace", |ui| {
                        let label = if self.workspace_manager.workspace_path.as_os_str().is_empty()
                        {
                            "No Workspace loaded".to_string()
                        } else {
                            self.workspace_manager.workspace.name.clone()
                        };
                        ui.add_enabled(
                            false,
                            egui::Label::new(
                                RichText::new(label)
                                    .color(egui::Color32::from_gray(230))
                                    .size(15.0),
                            ),
                        );
                        ui.separator();
                        if ui.button("New Workspace").clicked() {
                            self.open_workspace_dialog(WorkspaceDialogMode::New);
                            ui.close_menu();
                        }
                        if ui.button("Load Workspace").clicked() {
                            self.open_load_workspaces();
                            ui.close_menu();
                        }
                        if ui.button("Save Workspace").clicked() {
                            self.save_workspace_overwrite_current();
                            ui.close_menu();
                        }
                        let has_workspace =
                            !self.workspace_manager.workspace_path.as_os_str().is_empty();
                        if ui
                            .add_enabled(has_workspace, egui::Button::new("Export Workspace"))
                            .clicked()
                        {
                            self.export_workspace_path(
                                &self.workspace_manager.workspace_path.clone(),
                            );
                            ui.close_menu();
                        }
                        if ui
                            .add_enabled(has_workspace, egui::Button::new("Delete Workspace"))
                            .clicked()
                        {
                            self.show_confirm(
                                "Delete workspace",
                                "Delete current workspace?",
                                "Delete",
                                ConfirmAction::DeleteWorkspace(
                                    self.workspace_manager.workspace_path.clone(),
                                ),
                            );
                            ui.close_menu();
                        }
                        if ui.button("Manage Workspaces").clicked() {
                            self.open_manage_workspaces();
                            ui.close_menu();
                        }
                        ui.separator();
                        if ui.button("Clear Workspace").clicked() {
                            self.clear_workspace_to_default();
                            ui.close_menu();
                        }
                    });

                    ui.menu_button("Plugin", |ui| {
                        if ui.button("Load module").clicked() {
                            self.windows.runtime_node_dialog_mode =
                                RuntimeNodeDialogMode::LoadModule;
                            self.windows.runtime_node_dialog_target =
                                RuntimeNodeDialogTarget::Plugin;
                            self.windows.runtime_node_selected_index = None;
                            self.open_plugins();
                            ui.close_menu();
                        }
                        if ui.button("Add node").clicked() {
                            self.windows.runtime_node_dialog_mode = RuntimeNodeDialogMode::AddNode;
                            self.windows.runtime_node_dialog_target =
                                RuntimeNodeDialogTarget::Plugin;
                            self.windows.runtime_node_selected_index = None;
                            self.open_plugins();
                            ui.close_menu();
                        }
                        ui.separator();
                        if ui.button("New plugin").clicked() {
                            self.open_new_plugin_window_for_type(PluginKindType::Standard);
                            ui.close_menu();
                        }
                    });

                    ui.menu_button("Device", |ui| {
                        if ui.button("Load module").clicked() {
                            self.windows.runtime_node_dialog_mode =
                                RuntimeNodeDialogMode::LoadModule;
                            self.windows.runtime_node_dialog_target =
                                RuntimeNodeDialogTarget::Device;
                            self.windows.runtime_node_selected_index = None;
                            self.open_plugins();
                            ui.close_menu();
                        }
                        if ui.button("Add node").clicked() {
                            self.windows.runtime_node_dialog_mode = RuntimeNodeDialogMode::AddNode;
                            self.windows.runtime_node_dialog_target =
                                RuntimeNodeDialogTarget::Device;
                            self.windows.runtime_node_selected_index = None;
                            self.open_plugins();
                            ui.close_menu();
                        }
                        ui.separator();
                        if ui.button("New device").clicked() {
                            self.open_new_plugin_window_for_type(PluginKindType::Device);
                            ui.close_menu();
                        }
                    });

                    ui.menu_button("Connections", |ui| {
                        ui.set_width(220.0);
                        if ui.button("Manage connections").clicked() {
                            self.windows.manage_connections_open = true;
                            self.pending_window_focus = Some(WindowFocus::ManageConnections);
                            ui.close_menu();
                        }
                    });

                    ui.menu_button("View", |ui| {
                        ui.set_width(240.0);
                        let conn_icon = if self.connections_view_enabled {
                            "\u{f070}"
                        } else {
                            "\u{f06e}"
                        };
                        if ui
                            .button(format!("Toggle connections view {conn_icon}"))
                            .clicked()
                        {
                            self.connections_view_enabled = !self.connections_view_enabled;
                            ui.close_menu();
                        }
                        let state_icon = if matches!(self.view_mode, ViewMode::State) {
                            "\u{f205}"
                        } else {
                            "\u{f204}"
                        };
                        if ui
                            .button(format!("Toggle state machine view {state_icon}"))
                            .clicked()
                        {
                            self.view_mode = match self.view_mode {
                                ViewMode::Cards => ViewMode::State,
                                ViewMode::State => ViewMode::Cards,
                            };
                            ui.close_menu();
                        }

                        ui.separator();
                        let is_cards_view = matches!(self.view_mode, ViewMode::Cards);
                        if ui
                            .add_enabled(is_cards_view, egui::Button::new("Collapse all nodes"))
                            .clicked()
                        {
                            self.pending_plugin_sections_open = Some(false);
                            ui.close_menu();
                        }
                        if ui
                            .add_enabled(is_cards_view, egui::Button::new("Expand all nodes"))
                            .clicked()
                        {
                            self.pending_plugin_sections_open = Some(true);
                            ui.close_menu();
                        }
                        ui.menu_button("Order nodes", |ui| {
                            if ui.button("By name").clicked() {
                                self.pending_plugin_order = Some(PluginOrderMode::Name);
                                ui.close_menu();
                            }
                            if ui.button("By id").clicked() {
                                self.pending_plugin_order = Some(PluginOrderMode::Id);
                                ui.close_menu();
                            }
                            if ui.button("By priority").clicked() {
                                self.pending_plugin_order = Some(PluginOrderMode::Priority);
                                ui.close_menu();
                            }
                            if ui.button("By connections").clicked() {
                                self.pending_plugin_order = Some(PluginOrderMode::Connections);
                                ui.close_menu();
                            }
                        });
                    });

                    ui.menu_button("Tools", |ui| {
                        if ui.button("Measurements").clicked() {
                            self.measurements_open = true;
                            self.refresh_measurements();
                            ui.close_menu();
                        }
                        if ui.button("CSV telemetry").clicked() {
                            self.csv_telemetry_open = true;
                            ui.close_menu();
                        }
                        if ui.button("Plotter").clicked() {
                            self.runtime_plotter_config_open = true;
                            self.runtime_plotter_window_open = true;
                            ui.close_menu();
                        }
                        if ui.button("UML diagram").clicked() {
                            self.windows.uml_diagram_open = true;
                            self.pending_window_focus = Some(WindowFocus::UmlDiagram);
                            self.uml_text_buffer =
                                self.workspace_manager.current_workspace_uml_diagram();
                            self.uml_preview_hash = None;
                            self.uml_preview_error = None;
                            self.uml_preview_texture = None;
                            self.uml_preview_loading = false;
                            self.uml_preview_rx = None;
                            self.uml_export_svg = false;
                            self.uml_export_width = 1920;
                            self.uml_export_height = 1080;
                            self.uml_preview_zoom = 0.0;
                            ui.close_menu();
                        }
                    });

                    ui.menu_button("Runtime", |ui| {
                        if ui.button("Start all nodes").clicked() {
                            self.start_all_plugins();
                            ui.close_menu();
                        }
                        if ui.button("Stop all nodes").clicked() {
                            self.stop_all_plugins();
                            ui.close_menu();
                        }
                        ui.add(egui::Separator::default());
                        if ui.button("Settings").clicked() {
                            self.workspace_settings.open = true;
                            self.pending_window_focus = Some(WindowFocus::WorkspaceSettings);
                            ui.close_menu();
                        }
                    });
                    if ui.button("Help").clicked() {
                        self.help_state.open = true;
                        self.pending_window_focus = Some(WindowFocus::Help);
                    }

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.add_space(6.0);
                        ui.label(
                            RichText::new(format!("RTSyn {}", env!("CARGO_PKG_VERSION"))).weak(),
                        );
                    });
                });
            });
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.add_space(8.0);
            let panel_rect = ui.max_rect();
            if let Some(mode) = self.pending_plugin_order.take() {
                self.order_plugins_layout(panel_rect, mode);
            }

            // Reset connection click flag at start of frame
            self.connection_clicked_this_frame = false;

            match self.view_mode {
                ViewMode::Cards => {
                    // Three-phase rendering when highlight mode is active
                    if !matches!(self.highlight_mode, HighlightMode::None) {
                        // Phase 1: Render non-connected plugins
                        self.render_plugin_cards_filtered(ctx, panel_rect, Some(false));
                        // Phase 2: Render connections (now on Middle layer via Area)
                        self.render_connection_view(ctx, panel_rect);
                        // Phase 3: Render connected plugins
                        self.render_plugin_cards_filtered(ctx, panel_rect, Some(true));
                    } else {
                        // Normal rendering
                        self.render_plugin_cards(ctx, panel_rect);
                        self.render_connection_view(ctx, panel_rect);
                    }
                }
                ViewMode::State => {
                    if !matches!(self.highlight_mode, HighlightMode::None) {
                        // Phase 1: Render non-connected plugins
                        self.render_state_view_filtered(ctx, panel_rect, Some(false));
                        // Phase 2: Render connections
                        self.render_connection_view(ctx, panel_rect);
                        // Phase 3: Render connected plugins
                        self.render_state_view_filtered(ctx, panel_rect, Some(true));
                    } else {
                        self.render_state_view(ctx, panel_rect);
                        self.render_connection_view(ctx, panel_rect);
                    }
                }
            }
            self.pending_plugin_sections_open = None;

            if ctx.input(|i| i.pointer.primary_clicked()) {
                if let Some(pos) = ctx.input(|i| i.pointer.interact_pos()) {
                    let over_plugin = self.plugin_rects.values().any(|rect| rect.contains(pos));
                    // Check if over connection by testing distance to any connection
                    let over_connection = if !over_plugin {
                        self.workspace_manager
                            .workspace
                            .connections
                            .iter()
                            .any(|conn| {
                                if let (Some(from_rect), Some(to_rect)) = (
                                    self.plugin_rects.get(&conn.from_plugin),
                                    self.plugin_rects.get(&conn.to_plugin),
                                ) {
                                    let start = from_rect.center();
                                    let end = to_rect.center();
                                    distance_to_segment(pos, start, end) <= 10.0
                                } else {
                                    false
                                }
                            })
                    } else {
                        false
                    };
                    if !over_plugin && !over_connection && !self.connection_clicked_this_frame {
                        self.highlight_mode = HighlightMode::None;
                    }
                }
            }
        });

        // Apply pending highlight at end of frame (after rendering)
        if let Some(pending) = self.pending_highlight.take() {
            self.highlight_mode = pending;
        }

        self.render_workspace_dialog(ctx);
        self.render_load_workspaces_window(ctx);
        self.render_manage_workspaces_window(ctx);
        self.render_manage_plugins_window(ctx);
        self.render_install_plugins_window(ctx);
        self.render_uninstall_plugins_window(ctx);
        self.render_plugins_window(ctx);
        self.render_new_plugin_window(ctx);
        self.render_manage_connections_window(ctx);
        if self.connection_editor_host == ConnectionEditorHost::Main {
            self.render_connection_editor(ctx);
        }
        self.render_plugin_context_menu(ctx);
        self.render_connection_context_menu(ctx);
        self.render_plugin_config_window(ctx);
        self.render_plotter_windows(ctx);
        self.render_workspace_settings_window(ctx);
        self.render_uml_diagram_window(ctx);
        self.render_measurements_window(ctx);
        self.render_csv_telemetry_window(ctx);
        self.render_runtime_plotter_config_window(ctx);
        self.update_runtime_plotter(ctx);
        self.render_runtime_plotter_window(ctx);
        self.poll_runtime_plotter_export_dialog();
        self.render_daemon_restart_prompt(ctx);
        self.render_daemon_exit_prompt(ctx);
        self.render_help_window(ctx);
        self.render_build_dialog(ctx);
        self.render_confirm_remove_dialog(ctx);
        self.render_info_dialog(ctx);
        self.render_plotter_preview_dialog(ctx);
    }
}

impl GuiApp {
    fn render_daemon_restart_prompt(&mut self, ctx: &egui::Context) {
        if !self.daemon_restart_prompt_open {
            return;
        }

        let screen_rect = ctx.screen_rect();
        egui::Area::new(egui::Id::new("daemon_restart_modal_blocker"))
            .order(egui::Order::Middle)
            .fixed_pos(screen_rect.min)
            .show(ctx, |ui| {
                ui.allocate_rect(screen_rect, egui::Sense::click());
                ui.painter()
                    .rect_filled(screen_rect, 0.0, egui::Color32::from_black_alpha(190));
            });

        egui::Area::new(egui::Id::new("daemon_restart_modal"))
            .order(egui::Order::Foreground)
            .pivot(egui::Align2::CENTER_CENTER)
            .fixed_pos(screen_rect.center())
            .show(ctx, |ui| {
                egui::Frame::window(ui.style())
                    .rounding(egui::Rounding::same(6.0))
                    .show(ui, |ui| {
                        ui.set_width(380.0);
                        ui.heading("RTSyn daemon is running");
                        ui.label("A daemon is already available for this API endpoint.");
                        ui.label(
                            "Restart it for a clean GUI-owned session, or use the running daemon.",
                        );
                        if !self.daemon_status_text.is_empty() {
                            ui.add_space(6.0);
                            ui.label(RichText::new(&self.daemon_status_text).weak());
                        }
                        ui.add_space(10.0);
                        ui.horizontal(|ui| {
                            if ui.button("Use running daemon").clicked() {
                                match self.hydrate_runtime_nodes_from_daemon() {
                                    Ok(()) => {
                                        self.daemon_owned = false;
                                        self.daemon_restart_prompt_open = false;
                                        self.daemon_status_text = self.status.clone();
                                        self.show_info("RTSyn daemon", &self.status.clone());
                                    }
                                    Err(error) => {
                                        self.daemon_status_text = error;
                                    }
                                }
                            }
                            if ui.button("Restart daemon").clicked() {
                                match self.restart_gui_daemon() {
                                    Ok(message) => {
                                        self.daemon_owned = true;
                                        self.daemon_restart_prompt_open = false;
                                        self.daemon_status_text = message.clone();
                                        self.show_info("RTSyn daemon", &message);
                                    }
                                    Err(error) => {
                                        self.daemon_status_text =
                                            format!("Restart failed: {error}");
                                    }
                                }
                            }
                        });
                    });
            });
    }

    fn render_daemon_exit_prompt(&mut self, ctx: &egui::Context) {
        if !self.daemon_exit_prompt_open {
            return;
        }

        let screen_rect = ctx.screen_rect();
        egui::Area::new(egui::Id::new("daemon_exit_modal_blocker"))
            .order(egui::Order::Middle)
            .fixed_pos(screen_rect.min)
            .show(ctx, |ui| {
                ui.allocate_rect(screen_rect, egui::Sense::click());
                ui.painter()
                    .rect_filled(screen_rect, 0.0, egui::Color32::from_black_alpha(190));
            });

        egui::Area::new(egui::Id::new("daemon_exit_modal"))
            .order(egui::Order::Foreground)
            .pivot(egui::Align2::CENTER_CENTER)
            .fixed_pos(screen_rect.center())
            .show(ctx, |ui| {
                egui::Frame::window(ui.style())
                    .rounding(egui::Rounding::same(6.0))
                    .show(ui, |ui| {
                        ui.set_width(380.0);
                        ui.heading("Keep RTSyn daemon running?");
                        if self.daemon_owned {
                            ui.label("The GUI started this daemon for the current session.");
                        } else {
                            ui.label("The GUI is connected to an existing RTSyn daemon.");
                        }
                        ui.label("Choose whether it should keep running after the window closes.");
                        if !self.daemon_status_text.is_empty() {
                            ui.add_space(6.0);
                            ui.label(RichText::new(&self.daemon_status_text).weak());
                        }
                        ui.add_space(10.0);
                        ui.horizontal(|ui| {
                            if ui.button("Keep running").clicked() {
                                self.daemon_owned = false;
                                self.daemon_exit_prompt_open = false;
                                self.daemon_exit_allow_close = true;
                                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                            }
                            if ui.button("Stop daemon").clicked() {
                                let stop_result = self.daemon.as_ref().map(|daemon| daemon.stop());
                                match stop_result {
                                    Some(Ok(_)) | None => {
                                        self.daemon_owned = false;
                                        self.daemon_exit_prompt_open = false;
                                        self.daemon_exit_allow_close = true;
                                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                                    }
                                    Some(Err(error)) => {
                                        self.daemon_status_text = format!("Stop failed: {error}");
                                    }
                                }
                            }
                            if ui.button("Cancel").clicked() {
                                self.daemon_exit_prompt_open = false;
                            }
                        });
                    });
            });
    }

    fn restart_gui_daemon(&self) -> rtsyn_ui::Result<String> {
        let Some(daemon) = &self.daemon else {
            return Err(rtsyn_ui::Error::Api(
                "daemon controller is not available".to_string(),
            ));
        };
        daemon.stop()?;
        daemon.start()
    }

    fn poll_csv_telemetry_path_dialog(&mut self) {
        let result = match &self.csv_telemetry_path_dialog_rx {
            Some(rx) => rx.try_recv().ok(),
            None => None,
        };
        if let Some(path) = result {
            self.csv_telemetry_path_dialog_rx = None;
            if let Some(path) = path {
                self.csv_telemetry_path = path.to_string_lossy().to_string();
            }
        }
    }

    fn render_csv_telemetry_window(&mut self, ctx: &egui::Context) {
        if !self.csv_telemetry_open {
            return;
        }

        let source_options = self.csv_telemetry_source_options();
        self.normalize_csv_telemetry_selection(&source_options);
        self.prune_csv_telemetry_columns_for_current_workspace();

        let mut open = self.csv_telemetry_open;
        let mut write_clicked = false;
        let mut stop_clicked = false;
        let mut browse_clicked = false;
        let mut add_column_clicked = false;
        let mut remove_column = None;
        egui::Window::new("CSV telemetry")
            .open(&mut open)
            .default_width(520.0)
            .resizable(true)
            .show(ctx, |ui| {
                ui.label("Path");
                ui.horizontal(|ui| {
                    let mut path_text = self.csv_telemetry_path.clone();
                    if ui.add_sized(
                        [ui.available_width() - 76.0, 0.0],
                        egui::TextEdit::singleline(&mut path_text),
                    ).changed() {
                        self.csv_telemetry_path = path_text;
                    }
                    if ui.button("Browse").clicked() {
                        browse_clicked = true;
                    }
                });
                ui.add_space(8.0);
                ui.label("Columns");
                if self.csv_telemetry_columns.is_empty() {
                    ui.label("No columns configured.");
                } else {
                    for (index, column) in self.csv_telemetry_columns.iter().enumerate() {
                        ui.horizontal(|ui| {
                            ui.label(&column.name);
                            ui.add_space(6.0);
                            ui.label(RichText::new(&column.label).weak());
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui
                                        .add_enabled(
                                            !self.csv_telemetry_writing,
                                            egui::Button::new("Remove").small(),
                                        )
                                        .clicked()
                                    {
                                        remove_column = Some(index);
                                    }
                                },
                            );
                        });
                    }
                }
                ui.separator();
                ui.label("Add column");
                ui.horizontal(|ui| {
                    let selected_source_label = self
                        .selected_csv_telemetry_source(&source_options)
                        .map(|source| source.label.as_str())
                        .unwrap_or("No source available");
                    egui::ComboBox::from_id_source("csv_telemetry_source")
                        .selected_text(selected_source_label)
                        .width((ui.available_width() * 0.34).max(150.0))
                        .show_ui(ui, |ui| {
                            for source in &source_options {
                                if ui
                                    .selectable_value(
                                        &mut self.csv_telemetry_selected_source,
                                        Some(source.id.clone()),
                                        &source.label,
                                    )
                                    .clicked()
                                {
                                    self.csv_telemetry_selected_value =
                                        source.values.first().map(|value| value.key.clone());
                                    if let Some(value) = source.values.first() {
                                        self.csv_telemetry_new_column_name =
                                            value.default_column_name.clone();
                                    }
                                }
                            }
                        });
                    let selected_value_label = self
                        .selected_csv_telemetry_value(&source_options)
                        .map(|value| value.label.as_str())
                        .unwrap_or("No value available");
                    egui::ComboBox::from_id_source("csv_telemetry_value")
                        .selected_text(selected_value_label)
                        .width((ui.available_width() * 0.38).max(150.0))
                        .show_ui(ui, |ui| {
                            let selected_source_id = self.csv_telemetry_selected_source.clone();
                            let values = source_options
                                .iter()
                                .find(|source| {
                                    Some(source.id.as_str()) == selected_source_id.as_deref()
                                })
                                .map(|source| source.values.as_slice())
                                .unwrap_or(&[]);
                            for value in values {
                                if ui
                                    .selectable_value(
                                        &mut self.csv_telemetry_selected_value,
                                        Some(value.key.clone()),
                                        &value.label,
                                    )
                                    .clicked()
                                {
                                    self.csv_telemetry_new_column_name =
                                        value.default_column_name.clone();
                                }
                            }
                        });
                    ui.add_sized(
                        [ui.available_width() - 92.0, 0.0],
                        egui::TextEdit::singleline(&mut self.csv_telemetry_new_column_name)
                            .hint_text("column name"),
                    );
                    if ui
                        .add_enabled(!self.csv_telemetry_writing, egui::Button::new("Add"))
                        .clicked()
                    {
                        add_column_clicked = true;
                    }
                });
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(!self.csv_telemetry_writing, egui::Button::new("Write"))
                        .clicked()
                    {
                        write_clicked = true;
                    }
                    if ui
                        .add_enabled(self.csv_telemetry_writing, egui::Button::new("Stop"))
                        .clicked()
                    {
                        stop_clicked = true;
                    }
                });
                if !self.csv_telemetry_status.is_empty() {
                    ui.separator();
                    ui.label(&self.csv_telemetry_status);
                }
            });
        self.csv_telemetry_open = open;

        if browse_clicked {
            let (tx, rx) = mpsc::channel();
            let current_path = self.csv_telemetry_path.clone();
            self.csv_telemetry_path_dialog_rx = Some(rx);
            spawn_file_dialog_thread(move || {
                let file = save_file_dialog("CSV files", &["csv"], Some(&current_path));
                let _ = tx.send(file);
            });
        }

        if let Some(index) = remove_column {
            self.csv_telemetry_columns.remove(index);
        }

        if add_column_clicked {
            match self.build_csv_telemetry_column(&source_options) {
                Ok(column) => {
                    self.csv_telemetry_columns.push(column);
                    self.csv_telemetry_new_column_name.clear();
                    self.csv_telemetry_status.clear();
                }
                Err(error) => {
                    self.csv_telemetry_status = error;
                }
            }
        }

        if write_clicked {
            match Self::csv_telemetry_request_columns(&self.csv_telemetry_columns) {
                Ok((names, value_ids, measurement_fields)) => {
                    if !measurement_fields.is_empty()
                        && !matches!(
                            rtsyn_ui::api::ApiClient::default().csv_measurements_available(),
                            Ok(true)
                        )
                    {
                        self.csv_telemetry_status =
                            "Running API does not expose CSV measurement support".to_string();
                        self.show_info(
                            "CSV telemetry",
                            "The GUI daemon API is incompatible with CSV measurement columns.",
                        );
                        return;
                    }
                    match rtsyn_ui::api::ApiClient::default().configure_csv_telemetry_file(
                        &self.csv_telemetry_path,
                        &names,
                        &value_ids,
                        &measurement_fields,
                    ) {
                        Ok(response) if (200..300).contains(&response.status) => {
                            self.csv_telemetry_writing = true;
                            self.csv_telemetry_status =
                                format!("CSV telemetry configured: {}", self.csv_telemetry_path);
                            self.show_info("CSV telemetry", &self.csv_telemetry_status.clone());
                        }
                        Ok(response) => {
                            self.csv_telemetry_status = format!(
                                "Request failed: HTTP {} {}",
                                response.status, response.body
                            );
                        }
                        Err(error) => {
                            self.csv_telemetry_status = format!("Request failed: {error}");
                        }
                    }
                }
                Err(error) => {
                    self.csv_telemetry_status = error;
                }
            }
        }

        if stop_clicked {
            match rtsyn_ui::api::ApiClient::default().stop_csv_telemetry_file() {
                Ok(response) if (200..300).contains(&response.status) => {
                    self.csv_telemetry_writing = false;
                    self.csv_telemetry_status = "CSV telemetry stopped".to_string();
                    self.show_info("CSV telemetry", "CSV telemetry stopped");
                }
                Ok(response) => {
                    self.csv_telemetry_status = format!("Stop failed: HTTP {}", response.status);
                }
                Err(error) => {
                    self.csv_telemetry_status = format!("Stop failed: {error}");
                }
            }
        }
    }

    fn build_csv_telemetry_column(
        &self,
        source_options: &[CsvTelemetrySourceOption],
    ) -> Result<CsvTelemetryColumn, String> {
        let Some(source) = self.selected_csv_telemetry_source(source_options) else {
            return Err("Select a source".to_string());
        };
        let Some(option) = self.selected_csv_telemetry_value(source_options) else {
            return Err("Select a state or port value".to_string());
        };
        let name = self.csv_telemetry_new_column_name.trim();
        if name.is_empty() {
            return Err("Column name is required".to_string());
        }

        Ok(CsvTelemetryColumn {
            name: name.to_string(),
            source: option.source.clone(),
            label: format!("{} / {}", source.label, option.label),
        })
    }

    fn normalize_csv_telemetry_selection(&mut self, source_options: &[CsvTelemetrySourceOption]) {
        let source_valid = self
            .csv_telemetry_selected_source
            .as_deref()
            .is_some_and(|selected| source_options.iter().any(|source| source.id == selected));
        if !source_valid {
            self.csv_telemetry_selected_source =
                source_options.first().map(|source| source.id.clone());
        }

        let value_valid = self
            .selected_csv_telemetry_source(source_options)
            .and_then(|source| {
                self.csv_telemetry_selected_value
                    .as_deref()
                    .map(|selected| {
                        source
                            .values
                            .iter()
                            .any(|value| value.key.as_str() == selected)
                    })
            })
            .unwrap_or(false);
        if !value_valid {
            self.csv_telemetry_selected_value = self
                .selected_csv_telemetry_source(source_options)
                .and_then(|source| source.values.first())
                .map(|value| value.key.clone());
        }

        if self.csv_telemetry_new_column_name.trim().is_empty() {
            if let Some(value) = self.selected_csv_telemetry_value(source_options) {
                self.csv_telemetry_new_column_name = value.default_column_name.clone();
            }
        }
    }

    fn prune_csv_telemetry_columns_for_current_workspace(&mut self) {
        let live_node_ids: HashSet<u64> = self
            .workspace_manager
            .workspace
            .plugins
            .iter()
            .map(|plugin| plugin.id)
            .collect();
        let before = self.csv_telemetry_columns.len();
        self.csv_telemetry_columns
            .retain(|column| match &column.source {
                CsvTelemetryColumnSource::Value { node_id, .. } => live_node_ids.contains(node_id),
                CsvTelemetryColumnSource::Measurement(_) => true,
            });
        if before != self.csv_telemetry_columns.len() && self.csv_telemetry_writing {
            let _ = rtsyn_ui::api::ApiClient::default().stop_csv_telemetry_file();
            self.csv_telemetry_status =
                "CSV telemetry stopped after removing a selected node".to_string();
            self.csv_telemetry_writing = false;
        }
    }

    fn selected_csv_telemetry_source<'a>(
        &self,
        source_options: &'a [CsvTelemetrySourceOption],
    ) -> Option<&'a CsvTelemetrySourceOption> {
        let selected = self.csv_telemetry_selected_source.as_deref()?;
        source_options.iter().find(|source| source.id == selected)
    }

    fn selected_csv_telemetry_value<'a>(
        &self,
        source_options: &'a [CsvTelemetrySourceOption],
    ) -> Option<&'a CsvTelemetryValueOption> {
        let selected = self.csv_telemetry_selected_value.as_deref()?;
        self.selected_csv_telemetry_source(source_options)?
            .values
            .iter()
            .find(|value| value.key == selected)
    }

    fn csv_telemetry_source_options(&self) -> Vec<CsvTelemetrySourceOption> {
        let mut sources = vec![Self::csv_telemetry_measurement_source()];
        for plugin in &self.workspace_manager.workspace.plugins {
            let schema = self.parsed_display_schema_for_kind(&plugin.kind);
            let display_name = self
                .plugin_manager
                .installed_plugins
                .iter()
                .find(|installed| installed.manifest.kind == plugin.kind)
                .map(|installed| installed.manifest.name.as_str())
                .unwrap_or(plugin.kind.as_str());
            let mut values = Vec::new();
            Self::push_csv_telemetry_entries(
                &mut values,
                plugin.id,
                display_name,
                "input",
                &schema.inputs,
            );
            Self::push_csv_telemetry_entries(
                &mut values,
                plugin.id,
                display_name,
                "output",
                &schema.outputs,
            );
            Self::push_csv_telemetry_entries(
                &mut values,
                plugin.id,
                display_name,
                "state",
                &schema.variables,
            );
            if values.is_empty() {
                continue;
            }
            values.sort_by(|left, right| left.label.cmp(&right.label));
            sources.push(CsvTelemetrySourceOption {
                id: format!("node:{}", plugin.id),
                label: format!("#{} {display_name}", plugin.id),
                values,
            });
        }
        sources[1..].sort_by(|left, right| left.label.cmp(&right.label));
        sources
    }

    fn csv_telemetry_measurement_source() -> CsvTelemetrySourceOption {
        let values = [
            ("period_ns", "configured period"),
            ("actual_period_ns", "actual period"),
            ("latency_ns", "latency"),
            ("wake_lateness_ns", "wake lateness"),
            ("skipped_cycle_count", "skipped cycles"),
            ("missed_cycle", "missed cycle"),
            ("deadline_missed", "deadline missed"),
            ("devices_read_ns", "devices read"),
            ("plugins_time_ns", "plugins"),
            ("devices_write_ns", "devices write"),
        ]
        .into_iter()
        .map(|(field, label)| CsvTelemetryValueOption {
            key: format!("measurement:{field}"),
            label: label.to_string(),
            default_column_name: field.to_string(),
            source: CsvTelemetryColumnSource::Measurement(field.to_string()),
        })
        .collect();
        CsvTelemetrySourceOption {
            id: "measurements".to_string(),
            label: "Measurements".to_string(),
            values,
        }
    }

    fn push_csv_telemetry_entries(
        options: &mut Vec<CsvTelemetryValueOption>,
        node_id: u64,
        node_name: &str,
        kind: &str,
        entries: &[DisplayEntry],
    ) {
        let value_kind = match kind {
            "state" => CsvTelemetryValueKind::State,
            _ => CsvTelemetryValueKind::Port,
        };
        for entry in entries {
            let Ok(value_id) = entry.key.parse::<u32>() else {
                continue;
            };
            let default_column_name =
                Self::csv_column_name(&format!("{node_name}_{kind}_{}", entry.label));
            options.push(CsvTelemetryValueOption {
                key: format!("value:{node_id}:{kind}:{value_id}"),
                label: format!("{kind} {}", entry.label),
                default_column_name,
                source: CsvTelemetryColumnSource::Value {
                    node_id,
                    value_id,
                    kind: value_kind.clone(),
                },
            });
        }
    }

    fn csv_column_name(value: &str) -> String {
        let mut result = String::new();
        let mut previous_underscore = false;
        for character in value.chars() {
            if character.is_ascii_alphanumeric() {
                result.push(character.to_ascii_lowercase());
                previous_underscore = false;
            } else if !previous_underscore {
                result.push('_');
                previous_underscore = true;
            }
        }
        result.trim_matches('_').to_string()
    }

    fn csv_telemetry_request_columns(
        columns: &[CsvTelemetryColumn],
    ) -> Result<
        (
            Vec<String>,
            Vec<rtsyn_ui::api::CsvValueSelector>,
            Vec<String>,
        ),
        String,
    > {
        if columns.is_empty() {
            return Err("At least one column is required".to_string());
        }

        let mut names = Vec::with_capacity(columns.len());
        let mut values = Vec::new();
        let mut measurement_fields = Vec::new();
        for column in columns {
            let name = column.name.trim();
            if name.is_empty() {
                return Err("Column name is required".to_string());
            }
            names.push(name.to_string());
            match &column.source {
                CsvTelemetryColumnSource::Value {
                    node_id,
                    value_id,
                    kind,
                } => {
                    let node_id = u32::try_from(*node_id)
                        .map_err(|_| "Node id is out of range".to_string())?;
                    values.push(rtsyn_ui::api::CsvValueSelector {
                        node_id,
                        value_id: *value_id,
                        kind: match kind {
                            CsvTelemetryValueKind::Port => rtsyn_ui::api::CsvValueKind::Port,
                            CsvTelemetryValueKind::State => rtsyn_ui::api::CsvValueKind::State,
                        },
                    });
                }
                CsvTelemetryColumnSource::Measurement(field) => {
                    measurement_fields.push(field.clone());
                }
            }
        }

        Ok((names, values, measurement_fields))
    }

    fn render_runtime_plotter_config_window(&mut self, ctx: &egui::Context) {
        if !self.runtime_plotter_config_open {
            return;
        }

        let source_options = self.runtime_plotter_source_options();
        self.normalize_runtime_plotter_selection(&source_options);
        self.prune_runtime_plotter_series_for_current_workspace();

        let mut open = self.runtime_plotter_config_open;
        let mut add_series_clicked = false;
        let mut remove_series = None;
        let mut clear_clicked = false;
        let mut open_plotter_clicked = false;
        egui::Window::new("Runtime plotter")
            .open(&mut open)
            .default_width(560.0)
            .resizable(true)
            .show(ctx, |ui| {
                ui.label("Series");
                if self.runtime_plotter_series.is_empty() {
                    ui.label("No series configured.");
                } else {
                    for (index, series) in self.runtime_plotter_series.iter().enumerate() {
                        ui.horizontal(|ui| {
                            ui.label(&series.name);
                            ui.add_space(6.0);
                            ui.label(RichText::new(&series.label).weak());
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui.add(egui::Button::new("Remove").small()).clicked() {
                                        remove_series = Some(index);
                                    }
                                },
                            );
                        });
                    }
                }
                ui.separator();
                ui.label("Add series");
                ui.horizontal(|ui| {
                    let selected_source_label = self
                        .selected_runtime_plotter_source(&source_options)
                        .map(|source| source.label.as_str())
                        .unwrap_or("No source available");
                    egui::ComboBox::from_id_source("runtime_plotter_source")
                        .selected_text(selected_source_label)
                        .width((ui.available_width() * 0.34).max(150.0))
                        .show_ui(ui, |ui| {
                            for source in &source_options {
                                if ui
                                    .selectable_value(
                                        &mut self.runtime_plotter_selected_source,
                                        Some(source.id.clone()),
                                        &source.label,
                                    )
                                    .clicked()
                                {
                                    self.runtime_plotter_selected_value =
                                        source.values.first().map(|value| value.key.clone());
                                    if let Some(value) = source.values.first() {
                                        self.runtime_plotter_new_series_name =
                                            value.default_column_name.clone();
                                    }
                                }
                            }
                        });
                    let selected_value_label = self
                        .selected_runtime_plotter_value(&source_options)
                        .map(|value| value.label.as_str())
                        .unwrap_or("No value available");
                    egui::ComboBox::from_id_source("runtime_plotter_value")
                        .selected_text(selected_value_label)
                        .width((ui.available_width() * 0.38).max(150.0))
                        .show_ui(ui, |ui| {
                            let selected_source_id = self.runtime_plotter_selected_source.clone();
                            let values = source_options
                                .iter()
                                .find(|source| {
                                    Some(source.id.as_str()) == selected_source_id.as_deref()
                                })
                                .map(|source| source.values.as_slice())
                                .unwrap_or(&[]);
                            for value in values {
                                if ui
                                    .selectable_value(
                                        &mut self.runtime_plotter_selected_value,
                                        Some(value.key.clone()),
                                        &value.label,
                                    )
                                    .clicked()
                                {
                                    self.runtime_plotter_new_series_name =
                                        value.default_column_name.clone();
                                }
                            }
                        });
                    ui.add_sized(
                        [ui.available_width() - 92.0, 0.0],
                        egui::TextEdit::singleline(&mut self.runtime_plotter_new_series_name)
                            .hint_text("series name"),
                    );
                    if ui.button("Add").clicked() {
                        add_series_clicked = true;
                    }
                });
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Clear").clicked() {
                        clear_clicked = true;
                    }
                    if ui
                        .add_enabled(!self.runtime_plotter_window_open, egui::Button::new("Open"))
                        .clicked()
                    {
                        open_plotter_clicked = true;
                    }
                });
                if !self.runtime_plotter_status.is_empty() {
                    ui.separator();
                    ui.label(&self.runtime_plotter_status);
                }
            });
        self.runtime_plotter_config_open = open;

        if let Some(index) = remove_series {
            if let Some(series) = self.runtime_plotter_series.get(index).cloned() {
                self.set_runtime_plotter_subscription(&series.source, false);
            }
            self.runtime_plotter_series.remove(index);
            self.reconfigure_runtime_plotter();
        }

        if clear_clicked {
            let series = std::mem::take(&mut self.runtime_plotter_series);
            for entry in series {
                self.set_runtime_plotter_subscription(&entry.source, false);
            }
            self.runtime_plotter_status.clear();
            self.reconfigure_runtime_plotter();
        }

        if open_plotter_clicked {
            self.runtime_plotter_window_open = true;
        }

        if add_series_clicked {
            match self.build_runtime_plotter_series(&source_options) {
                Ok(series) => {
                    if self
                        .runtime_plotter_series
                        .iter()
                        .any(|item| item == &series)
                    {
                        self.runtime_plotter_status = "Series is already configured".to_string();
                    } else if self.set_runtime_plotter_subscription(&series.source, true) {
                        self.runtime_plotter_series.push(series);
                        self.runtime_plotter_new_series_name.clear();
                        self.runtime_plotter_status.clear();
                        self.runtime_plotter_window_open = true;
                        self.reconfigure_runtime_plotter();
                    }
                }
                Err(error) => {
                    self.runtime_plotter_status = error;
                }
            }
        }
    }

    fn render_runtime_plotter_window(&mut self, ctx: &egui::Context) {
        if !self.runtime_plotter_window_open {
            return;
        }

        let viewport_id = egui::ViewportId::from_hash_of("runtime_plotter_graph");
        let builder = egui::ViewportBuilder::default()
            .with_title("Runtime plotter")
            .with_min_inner_size([1100.0, 650.0])
            .with_resizable(true)
            .with_close_button(true);
        let mut close_requested = false;

        ctx.show_viewport_immediate(viewport_id, builder, |ctx, class| {
            if class == egui::ViewportClass::Embedded {
                return;
            }
            if ctx.input(|input| input.viewport().close_requested()) {
                close_requested = true;
            }

            egui::CentralPanel::default().show(ctx, |ui| {
                if self.runtime_plotter_series.is_empty() {
                    ui.label("Add port or state values from Tools > Plotter.");
                    return;
                }

                ui.horizontal(|ui| {
                    ui.label("Refresh Hz");
                    ui.add(
                        egui::DragValue::new(&mut self.runtime_plotter_settings.refresh_hz)
                            .clamp_range(0.2..=60.0)
                            .speed(0.2),
                    );
                    ui.separator();
                    if ui.button("Preview/export").clicked() {
                        self.prepare_runtime_plotter_preview_state();
                        self.runtime_plotter_settings.open = true;
                    }
                });
                ui.separator();

                let mut save_requested = false;
                self.render_runtime_plotter_graph(ui, "");
                if self.runtime_plotter_settings.open {
                    save_requested = self.render_runtime_plotter_preview_overlay(ctx);
                }
                if save_requested {
                    self.request_runtime_plotter_export();
                }
            });
        });

        if close_requested {
            self.runtime_plotter_window_open = false;
        }
    }

    fn render_runtime_plotter_graph(&mut self, ui: &mut egui::Ui, title: &str) {
        self.runtime_plotter
            .set_window_ms(self.runtime_plotter_settings.window_ms);
        self.prepare_runtime_plotter_preview_state();
        let series_transforms = Self::runtime_plotter_series_transforms(
            &self.runtime_plotter_settings.series_scales,
            &self.runtime_plotter_settings.series_offsets,
            self.runtime_plotter_settings.series_names.len(),
        );
        self.runtime_plotter.render_with_settings(
            ui,
            title,
            &self.runtime_plotter_settings.x_axis_name,
            self.runtime_plotter_settings.show_axes,
            self.runtime_plotter_settings.show_legend,
            self.runtime_plotter_settings.show_grid,
            Some(&self.runtime_plotter_settings.title),
            Some(&self.runtime_plotter_settings.series_names),
            Some(&series_transforms),
            Some(&self.runtime_plotter_settings.colors),
            self.runtime_plotter_settings.dark_theme,
            Some(&self.runtime_plotter_settings.x_axis_name),
            Some(&self.runtime_plotter_settings.y_axis_name),
            Some(self.runtime_plotter_settings.window_ms),
        );
    }

    fn render_runtime_plotter_preview_overlay(&mut self, ctx: &egui::Context) -> bool {
        if !self.runtime_plotter_settings.open {
            return false;
        }

        self.prepare_runtime_plotter_preview_state();
        let mut save_requested = false;
        let mut open = self.runtime_plotter_settings.open;
        egui::Window::new("Runtime Plot Preview & Export")
            .open(&mut open)
            .resizable(true)
            .default_size(egui::vec2(680.0, 560.0))
            .show(ctx, |ui| {
                ui.vertical(|ui| {
                    ui.horizontal(|ui| {
                        ui.label("Title:");
                        ui.text_edit_singleline(&mut self.runtime_plotter_settings.title);
                    });

                    ui.horizontal(|ui| {
                        ui.checkbox(&mut self.runtime_plotter_settings.show_axes, "Show axes");
                        ui.checkbox(
                            &mut self.runtime_plotter_settings.show_legend,
                            "Show legend",
                        );
                        ui.checkbox(&mut self.runtime_plotter_settings.show_grid, "Show grid");
                        ui.checkbox(&mut self.runtime_plotter_settings.dark_theme, "Dark theme");
                    });

                    ui.horizontal(|ui| {
                        ui.label("X-axis:");
                        ui.text_edit_singleline(&mut self.runtime_plotter_settings.x_axis_name);
                        ui.label("Y-axis:");
                        ui.text_edit_singleline(&mut self.runtime_plotter_settings.y_axis_name);
                    });
                    Self::render_runtime_plotter_timebase_controls(
                        ui,
                        &mut self.runtime_plotter_settings.window_ms,
                        &mut self.runtime_plotter_settings.timebase_divisions,
                    );

                    ui.separator();
                    ui.label("Series customization:");
                    egui::ScrollArea::vertical()
                        .max_height(150.0)
                        .show(ui, |ui| {
                            for i in 0..self.runtime_plotter_settings.series_names.len() {
                                ui.horizontal(|ui| {
                                    ui.label(format!("Series {}:", i + 1));
                                    ui.text_edit_singleline(
                                        &mut self.runtime_plotter_settings.series_names[i],
                                    );
                                    ui.menu_button("Tune", |ui| {
                                        ui.set_min_width(220.0);
                                        Self::render_runtime_plotter_series_wheels(
                                            ui,
                                            &mut self.runtime_plotter_settings.series_scales[i],
                                            &mut self.runtime_plotter_settings.series_offsets[i],
                                        );
                                    });
                                    ui.color_edit_button_srgba(
                                        &mut self.runtime_plotter_settings.colors[i],
                                    );
                                });
                            }
                        });

                    ui.separator();
                    ui.label("Preview:");
                    let preview_size = egui::vec2(ui.available_width(), 220.0);
                    let series_transforms = Self::runtime_plotter_series_transforms(
                        &self.runtime_plotter_settings.series_scales,
                        &self.runtime_plotter_settings.series_offsets,
                        self.runtime_plotter_settings.series_names.len(),
                    );
                    ui.allocate_ui(preview_size, |ui| {
                        self.runtime_plotter
                            .set_window_ms(self.runtime_plotter_settings.window_ms);
                        self.runtime_plotter.render_with_settings(
                            ui,
                            "Preview",
                            &self.runtime_plotter_settings.x_axis_name,
                            self.runtime_plotter_settings.show_axes,
                            self.runtime_plotter_settings.show_legend,
                            self.runtime_plotter_settings.show_grid,
                            Some(&self.runtime_plotter_settings.title),
                            Some(&self.runtime_plotter_settings.series_names),
                            Some(&series_transforms),
                            Some(&self.runtime_plotter_settings.colors),
                            self.runtime_plotter_settings.dark_theme,
                            Some(&self.runtime_plotter_settings.x_axis_name),
                            Some(&self.runtime_plotter_settings.y_axis_name),
                            Some(self.runtime_plotter_settings.window_ms),
                        );
                    });

                    ui.separator();
                    ui.horizontal(|ui| {
                        ui.checkbox(
                            &mut self.runtime_plotter_settings.export_svg,
                            "Export as SVG",
                        );
                        ui.checkbox(
                            &mut self.runtime_plotter_settings.high_quality,
                            "High quality PNG",
                        );
                    });
                    ui.horizontal(|ui| {
                        ui.label("Resolution:");
                        let old_width = self.runtime_plotter_settings.width;
                        ui.add_enabled(
                            !self.runtime_plotter_settings.export_svg,
                            egui::DragValue::new(&mut self.runtime_plotter_settings.width)
                                .clamp_range(400..=4000)
                                .suffix("px"),
                        );
                        if self.runtime_plotter_settings.width != old_width
                            && !self.runtime_plotter_settings.export_svg
                        {
                            let ratio = 16.0 / 9.0;
                            self.runtime_plotter_settings.height =
                                (self.runtime_plotter_settings.width as f32 / ratio) as u32;
                        }

                        ui.label("x");
                        let old_height = self.runtime_plotter_settings.height;
                        ui.add_enabled(
                            !self.runtime_plotter_settings.export_svg,
                            egui::DragValue::new(&mut self.runtime_plotter_settings.height)
                                .clamp_range(300..=3000)
                                .suffix("px"),
                        );
                        if self.runtime_plotter_settings.height != old_height
                            && !self.runtime_plotter_settings.export_svg
                        {
                            let ratio = 16.0 / 9.0;
                            self.runtime_plotter_settings.width =
                                (self.runtime_plotter_settings.height as f32 * ratio) as u32;
                        }
                        if ui.button("16:9").clicked() && !self.runtime_plotter_settings.export_svg
                        {
                            let ratio = 16.0 / 9.0;
                            self.runtime_plotter_settings.height =
                                (self.runtime_plotter_settings.width as f32 / ratio) as u32;
                        }
                    });
                    ui.horizontal(|ui| {
                        if ui.button("Save picture").clicked() {
                            save_requested = true;
                        }
                    });
                });
            });
        self.runtime_plotter_settings.open = open;
        save_requested
    }

    fn prepare_runtime_plotter_preview_state(&mut self) {
        let target = self.runtime_plotter_series.len();
        let default_names = self
            .runtime_plotter_series
            .iter()
            .map(|series| series.name.clone())
            .collect::<Vec<_>>();
        if self.runtime_plotter_settings.series_names.len() < target {
            for idx in self.runtime_plotter_settings.series_names.len()..target {
                self.runtime_plotter_settings.series_names.push(
                    default_names
                        .get(idx)
                        .cloned()
                        .unwrap_or_else(|| format!("Series {}", idx + 1)),
                );
                self.runtime_plotter_settings.series_scales.push(1.0);
                self.runtime_plotter_settings.series_offsets.push(0.0);
                self.runtime_plotter_settings.colors.push(
                    [
                        egui::Color32::from_rgb(86, 156, 214),
                        egui::Color32::from_rgb(220, 122, 95),
                        egui::Color32::from_rgb(181, 206, 168),
                        egui::Color32::from_rgb(197, 134, 192),
                        egui::Color32::from_rgb(220, 220, 170),
                        egui::Color32::from_rgb(156, 220, 254),
                        egui::Color32::from_rgb(255, 204, 102),
                        egui::Color32::from_rgb(206, 145, 120),
                        egui::Color32::from_rgb(78, 201, 176),
                        egui::Color32::from_rgb(214, 157, 133),
                    ][idx % 10],
                );
            }
        } else {
            self.runtime_plotter_settings.series_names.truncate(target);
            self.runtime_plotter_settings.series_scales.truncate(target);
            self.runtime_plotter_settings
                .series_offsets
                .truncate(target);
            self.runtime_plotter_settings.colors.truncate(target);
        }
        for (idx, name) in default_names.into_iter().enumerate() {
            if let Some(current) = self.runtime_plotter_settings.series_names.get_mut(idx) {
                if current.trim().is_empty() || current.starts_with("Series ") {
                    *current = name;
                }
            }
        }
    }

    fn render_runtime_plotter_timebase_controls(
        ui: &mut egui::Ui,
        window_ms: &mut f64,
        divisions: &mut u32,
    ) {
        ui.horizontal(|ui| {
            ui.label("Time window:");
            ui.add(
                egui::DragValue::new(window_ms)
                    .clamp_range(100.0..=300_000.0)
                    .speed(100.0)
                    .suffix(" ms"),
            );
            ui.label("Divisions:");
            ui.add(
                egui::DragValue::new(divisions)
                    .clamp_range(1..=200)
                    .speed(1.0),
            );
        });
    }

    fn render_runtime_plotter_series_wheels(ui: &mut egui::Ui, scale: &mut f64, offset: &mut f64) {
        ui.horizontal(|ui| {
            ui.label("Scale");
            ui.add(egui::DragValue::new(scale).speed(0.05));
        });
        ui.horizontal(|ui| {
            ui.label("Offset");
            ui.add(egui::DragValue::new(offset).speed(0.05));
        });
        ui.horizontal(|ui| {
            if ui.button("Reset").clicked() {
                *scale = 1.0;
                *offset = 0.0;
            }
        });
    }

    fn runtime_plotter_series_transforms(
        scales: &[f64],
        offsets: &[f64],
        count: usize,
    ) -> Vec<crate::gui::plotter::SeriesTransform> {
        (0..count)
            .map(|idx| crate::gui::plotter::SeriesTransform {
                scale: *scales.get(idx).unwrap_or(&1.0),
                offset: *offsets.get(idx).unwrap_or(&0.0),
            })
            .collect()
    }

    fn request_runtime_plotter_export(&mut self) {
        if self.runtime_plotter_export_rx.is_some() {
            return;
        }
        self.prepare_runtime_plotter_preview_state();
        let extension = if self.runtime_plotter_settings.export_svg {
            "svg"
        } else {
            "png"
        };
        let filter_name = if self.runtime_plotter_settings.export_svg {
            "SVG"
        } else {
            "PNG"
        };
        let base_name = Self::csv_column_name(&self.runtime_plotter_settings.title);
        let base_name = if base_name.is_empty() {
            "runtime_plotter".to_string()
        } else {
            base_name
        };
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let default_name = format!("{base_name}-{now}.{extension}");
        let (tx, rx) = mpsc::channel();
        self.runtime_plotter_export_rx = Some(rx);
        spawn_file_dialog_thread(move || {
            let file = if has_rt_capabilities() {
                zenity_file_dialog("save", Some(&format!("*.{extension}")))
            } else {
                rfd::FileDialog::new()
                    .add_filter(filter_name, &[extension])
                    .set_file_name(&default_name)
                    .save_file()
            };
            let _ = tx.send(file);
        });
    }

    fn poll_runtime_plotter_export_dialog(&mut self) {
        let result = match &self.runtime_plotter_export_rx {
            Some(rx) => rx.try_recv().ok(),
            None => None,
        };
        let Some(selection) = result else {
            return;
        };
        self.runtime_plotter_export_rx = None;
        let Some(path) = selection else {
            return;
        };
        self.prepare_runtime_plotter_preview_state();
        let transforms = Self::runtime_plotter_series_transforms(
            &self.runtime_plotter_settings.series_scales,
            &self.runtime_plotter_settings.series_offsets,
            self.runtime_plotter_settings.series_names.len(),
        );
        let result = if self.runtime_plotter_settings.export_svg {
            self.runtime_plotter.export_svg_with_settings(
                &path,
                &self.runtime_plotter_settings.x_axis_name,
                self.runtime_plotter_settings.show_axes,
                self.runtime_plotter_settings.show_legend,
                self.runtime_plotter_settings.show_grid,
                &self.runtime_plotter_settings.title,
                &self.runtime_plotter_settings.series_names,
                &transforms,
                &self.runtime_plotter_settings.colors,
                self.runtime_plotter_settings.dark_theme,
                &self.runtime_plotter_settings.x_axis_name,
                &self.runtime_plotter_settings.y_axis_name,
                self.runtime_plotter_settings.window_ms,
                self.runtime_plotter_settings.width,
                self.runtime_plotter_settings.height,
            )
        } else if self.runtime_plotter_settings.high_quality {
            self.runtime_plotter.export_png_hq_with_settings(
                &path,
                &self.runtime_plotter_settings.x_axis_name,
                self.runtime_plotter_settings.show_axes,
                self.runtime_plotter_settings.show_legend,
                self.runtime_plotter_settings.show_grid,
                &self.runtime_plotter_settings.title,
                &self.runtime_plotter_settings.series_names,
                &transforms,
                &self.runtime_plotter_settings.colors,
                self.runtime_plotter_settings.dark_theme,
                &self.runtime_plotter_settings.x_axis_name,
                &self.runtime_plotter_settings.y_axis_name,
                self.runtime_plotter_settings.window_ms,
            )
        } else {
            self.runtime_plotter.export_png_with_settings(
                &path,
                &self.runtime_plotter_settings.x_axis_name,
                self.runtime_plotter_settings.show_axes,
                self.runtime_plotter_settings.show_legend,
                self.runtime_plotter_settings.show_grid,
                &self.runtime_plotter_settings.title,
                &self.runtime_plotter_settings.series_names,
                &transforms,
                &self.runtime_plotter_settings.colors,
                self.runtime_plotter_settings.dark_theme,
                &self.runtime_plotter_settings.x_axis_name,
                &self.runtime_plotter_settings.y_axis_name,
                self.runtime_plotter_settings.window_ms,
                self.runtime_plotter_settings.width,
                self.runtime_plotter_settings.height,
            )
        };

        match result {
            Ok(()) => {
                self.runtime_plotter_status = format!("Saved plot: {}", path.display());
                self.show_info("Runtime plotter", &self.runtime_plotter_status.clone());
            }
            Err(error) => {
                self.runtime_plotter_status = format!("Export failed: {error}");
                self.show_info("Runtime plotter", &self.runtime_plotter_status.clone());
            }
        }
    }

    fn update_runtime_plotter(&mut self, ctx: &egui::Context) {
        if !self.runtime_plotter_window_open || self.runtime_plotter_series.is_empty() {
            return;
        }
        let refresh_hz = self.runtime_plotter_settings.refresh_hz.max(0.2);
        let refresh_interval = Duration::from_secs_f64(1.0 / refresh_hz);
        if self.runtime_plotter_last_update.elapsed() < refresh_interval {
            ctx.request_repaint_after(refresh_interval);
            return;
        }
        self.runtime_plotter_last_update = Instant::now();
        ctx.request_repaint_after(refresh_interval);

        self.push_runtime_plotter_measurement_sample();
    }

    fn ingest_runtime_plotter_telemetry(&mut self, samples: &[RuntimeTelemetrySample]) {
        if !self.runtime_plotter_window_open || self.runtime_plotter_series.is_empty() {
            return;
        }
        if self.runtime_plotter_latest_values.len() != self.runtime_plotter_series.len() {
            self.runtime_plotter_latest_values = vec![0.0; self.runtime_plotter_series.len()];
        }

        let mut pushed = false;
        for sample in samples {
            let Some(index) = self
                .runtime_plotter_series
                .iter()
                .position(|series| Self::runtime_plotter_series_matches_sample(series, sample))
            else {
                continue;
            };
            self.runtime_plotter_latest_values[index] = sample.value;
            self.runtime_plotter_tick = self.runtime_plotter_tick.saturating_add(1);
            if sample.timestamp_ns > 0 {
                self.runtime_plotter.push_sample(
                    self.runtime_plotter_tick,
                    sample.timestamp_ns as f64 / 1_000_000_000.0,
                    1000.0,
                    &self.runtime_plotter_latest_values,
                );
            } else {
                self.runtime_plotter.push_sample_from_tick(
                    self.runtime_plotter_tick,
                    self.state_sync.logic_period_seconds.max(0.001),
                    1000.0,
                    &self.runtime_plotter_latest_values,
                );
            }
            pushed = true;
        }
        if pushed {
            self.runtime_plotter_status.clear();
        }
    }

    fn runtime_plotter_series_matches_sample(
        series: &CsvTelemetryColumn,
        sample: &RuntimeTelemetrySample,
    ) -> bool {
        let CsvTelemetryColumnSource::Value {
            node_id,
            value_id,
            kind,
        } = &series.source
        else {
            return false;
        };
        let expected_kind = match kind {
            CsvTelemetryValueKind::Port => "port",
            CsvTelemetryValueKind::State => "state",
        };
        *node_id == sample.node_id
            && *value_id == sample.value_id
            && sample.kind.as_str() == expected_kind
    }

    fn push_runtime_plotter_measurement_sample(&mut self) {
        if !self
            .runtime_plotter_series
            .iter()
            .any(|series| matches!(series.source, CsvTelemetryColumnSource::Measurement(_)))
        {
            return;
        }
        let Some(measurement) = self.runtime_plotter_measurement() else {
            return;
        };
        if self.runtime_plotter_latest_values.len() != self.runtime_plotter_series.len() {
            self.runtime_plotter_latest_values = vec![0.0; self.runtime_plotter_series.len()];
        }

        let mut updated = false;
        for (index, series) in self.runtime_plotter_series.iter().enumerate() {
            if let CsvTelemetryColumnSource::Measurement(field) = &series.source {
                if let Some(value) = measurement.get(field) {
                    self.runtime_plotter_latest_values[index] = Self::plotter_value_as_f64(value);
                    updated = true;
                }
            }
        }
        if !updated {
            return;
        }
        let tick = measurement
            .get("cycle_id")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or_else(|| {
                self.runtime_plotter_tick = self.runtime_plotter_tick.saturating_add(1);
                self.runtime_plotter_tick
            });
        self.runtime_plotter_tick = self.runtime_plotter_tick.max(tick);
        let timestamp_ns = measurement
            .get("timestamp_ns")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or_default();
        if timestamp_ns > 0 {
            self.runtime_plotter.push_sample(
                self.runtime_plotter_tick,
                timestamp_ns as f64 / 1_000_000_000.0,
                1000.0,
                &self.runtime_plotter_latest_values,
            );
        } else {
            self.runtime_plotter.push_sample_from_tick(
                self.runtime_plotter_tick,
                self.state_sync.logic_period_seconds.max(0.001),
                1000.0,
                &self.runtime_plotter_latest_values,
            );
        }
        self.runtime_plotter_status.clear();
    }

    fn build_runtime_plotter_series(
        &self,
        source_options: &[CsvTelemetrySourceOption],
    ) -> Result<CsvTelemetryColumn, String> {
        let Some(source) = self.selected_runtime_plotter_source(source_options) else {
            return Err("Select a source".to_string());
        };
        let Some(option) = self.selected_runtime_plotter_value(source_options) else {
            return Err("Select a state or port value".to_string());
        };
        let name = self.runtime_plotter_new_series_name.trim();
        if name.is_empty() {
            return Err("Series name is required".to_string());
        }

        Ok(CsvTelemetryColumn {
            name: name.to_string(),
            source: option.source.clone(),
            label: format!("{} / {}", source.label, option.label),
        })
    }

    fn normalize_runtime_plotter_selection(&mut self, source_options: &[CsvTelemetrySourceOption]) {
        let source_valid = self
            .runtime_plotter_selected_source
            .as_deref()
            .is_some_and(|selected| source_options.iter().any(|source| source.id == selected));
        if !source_valid {
            self.runtime_plotter_selected_source =
                source_options.first().map(|source| source.id.clone());
        }

        let value_valid = self
            .selected_runtime_plotter_source(source_options)
            .and_then(|source| {
                self.runtime_plotter_selected_value
                    .as_deref()
                    .map(|selected| {
                        source
                            .values
                            .iter()
                            .any(|value| value.key.as_str() == selected)
                    })
            })
            .unwrap_or(false);
        if !value_valid {
            self.runtime_plotter_selected_value = self
                .selected_runtime_plotter_source(source_options)
                .and_then(|source| source.values.first())
                .map(|value| value.key.clone());
        }

        if self.runtime_plotter_new_series_name.trim().is_empty() {
            if let Some(value) = self.selected_runtime_plotter_value(source_options) {
                self.runtime_plotter_new_series_name = value.default_column_name.clone();
            }
        }
    }

    fn prune_runtime_plotter_series_for_current_workspace(&mut self) {
        let live_node_ids: HashSet<u64> = self
            .workspace_manager
            .workspace
            .plugins
            .iter()
            .map(|plugin| plugin.id)
            .collect();
        let mut removed_sources = Vec::new();
        self.runtime_plotter_series
            .retain(|series| match &series.source {
                CsvTelemetryColumnSource::Value { node_id, .. }
                    if live_node_ids.contains(node_id) =>
                {
                    true
                }
                CsvTelemetryColumnSource::Value { .. } => {
                    removed_sources.push(series.source.clone());
                    false
                }
                CsvTelemetryColumnSource::Measurement(_) => true,
            });
        for source in removed_sources {
            self.set_runtime_plotter_subscription(&source, false);
        }
    }

    fn selected_runtime_plotter_source<'a>(
        &self,
        source_options: &'a [CsvTelemetrySourceOption],
    ) -> Option<&'a CsvTelemetrySourceOption> {
        let selected = self.runtime_plotter_selected_source.as_deref()?;
        source_options.iter().find(|source| source.id == selected)
    }

    fn selected_runtime_plotter_value<'a>(
        &self,
        source_options: &'a [CsvTelemetrySourceOption],
    ) -> Option<&'a CsvTelemetryValueOption> {
        let selected = self.runtime_plotter_selected_value.as_deref()?;
        self.selected_runtime_plotter_source(source_options)?
            .values
            .iter()
            .find(|value| value.key == selected)
    }

    fn runtime_plotter_source_options(&self) -> Vec<CsvTelemetrySourceOption> {
        self.csv_telemetry_source_options()
    }

    fn reconfigure_runtime_plotter(&mut self) {
        let series_count = self.runtime_plotter_series.len();
        self.runtime_plotter
            .set_window_ms(self.runtime_plotter_settings.window_ms);
        self.runtime_plotter.update_config(
            series_count,
            self.runtime_plotter_settings.refresh_hz.max(0.2),
            self.state_sync.logic_period_seconds.max(0.001),
        );
        self.runtime_plotter.set_series_names(
            self.runtime_plotter_series
                .iter()
                .map(|series| series.name.clone())
                .collect(),
        );
    }

    fn set_runtime_plotter_subscription(
        &mut self,
        source: &CsvTelemetryColumnSource,
        send: bool,
    ) -> bool {
        let CsvTelemetryColumnSource::Value {
            node_id,
            value_id,
            kind,
        } = source
        else {
            return true;
        };
        let Ok(node_id) = u32::try_from(*node_id) else {
            self.runtime_plotter_status = "Node id is out of range".to_string();
            return false;
        };
        let Some(mask) = 1_u64.checked_shl(*value_id) else {
            self.runtime_plotter_status = "Value id is out of range".to_string();
            return false;
        };
        let response = match kind {
            CsvTelemetryValueKind::Port => {
                rtsyn_ui::api::ApiClient::default().subscribe_port_values(node_id, send, mask)
            }
            CsvTelemetryValueKind::State => {
                rtsyn_ui::api::ApiClient::default().subscribe_node_states(node_id, send, mask)
            }
        };
        match response {
            Ok(response) if (200..300).contains(&response.status) => true,
            Ok(response) => {
                self.runtime_plotter_status = format!(
                    "Subscription failed: HTTP {} {}",
                    response.status, response.body
                );
                false
            }
            Err(error) => {
                self.runtime_plotter_status = format!("Subscription failed: {error}");
                false
            }
        }
    }

    fn runtime_plotter_measurement(&mut self) -> Option<serde_json::Value> {
        match rtsyn_ui::api::ApiClient::default().measurements() {
            Ok(response) if (200..300).contains(&response.status) => {
                serde_json::from_str::<serde_json::Value>(&response.body).ok()
            }
            Ok(response) => {
                self.runtime_plotter_status =
                    format!("Measurement values failed: HTTP {}", response.status);
                None
            }
            Err(error) => {
                self.runtime_plotter_status = format!("Measurement values failed: {error}");
                None
            }
        }
    }

    fn plotter_value_as_f64(value: &serde_json::Value) -> f64 {
        if let Some(value) = value.as_f64() {
            value
        } else if let Some(value) = value.as_bool() {
            if value {
                1.0
            } else {
                0.0
            }
        } else if let Some(value) = value.as_str() {
            value.parse::<f64>().unwrap_or(0.0)
        } else {
            0.0
        }
    }

    fn refresh_measurements(&mut self) {
        match rtsyn_ui::api::ApiClient::default().measurements() {
            Ok(response) if (200..300).contains(&response.status) => {
                self.measurements_text = response.body;
            }
            Ok(response) => {
                self.measurements_text =
                    serde_json::json!({"available": false, "error": format!("HTTP {}", response.status)})
                        .to_string();
            }
            Err(error) => {
                self.measurements_text =
                    serde_json::json!({"available": false, "error": error.to_string()}).to_string();
            }
        }
        self.last_measurements_update = Instant::now();
    }

    fn render_measurements_window(&mut self, ctx: &egui::Context) {
        if !self.measurements_open {
            return;
        }

        if self.last_measurements_update.elapsed() >= Duration::from_millis(500) {
            self.refresh_measurements();
        }
        ctx.request_repaint_after(Duration::from_millis(500));

        let measurement = serde_json::from_str::<serde_json::Value>(&self.measurements_text).ok();

        egui::Window::new("Measurements")
            .open(&mut self.measurements_open)
            .default_width(380.0)
            .resizable(true)
            .show(ctx, |ui| {
                if let Some(measurement) = measurement.as_ref() {
                    Self::render_measurement_summary(ui, measurement);
                } else {
                    ui.label(RichText::new("Invalid measurement response").strong());
                }
            });
    }

    fn render_measurement_summary(ui: &mut egui::Ui, measurement: &serde_json::Value) {
        let available = measurement
            .get("available")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);

        if !available {
            let error = measurement
                .get("error")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("No measurements available yet");
            ui.label(RichText::new(error).strong());
            return;
        }

        let cycle_id = measurement
            .get("cycle_id")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or_default();
        let skipped_cycle_count = measurement
            .get("skipped_cycle_count")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or_default();

        ui.horizontal(|ui| {
            ui.label(RichText::new(format!("Cycle {cycle_id}")).strong());
            ui.add_space(8.0);
            let status = if skipped_cycle_count > 0 {
                RichText::new("skipped cycle").color(egui::Color32::from_rgb(235, 90, 90))
            } else {
                RichText::new("no skipped cycles").color(egui::Color32::from_rgb(80, 200, 120))
            };
            ui.label(status);
        });

        ui.add_space(8.0);
        Self::render_measurement_section(
            ui,
            "\u{f017}  Cycle timing",
            &[
                (
                    "configured period",
                    Self::measurement_ns(measurement, "period_ns"),
                ),
                (
                    "actual period",
                    Self::measurement_ns(measurement, "actual_period_ns"),
                ),
                (
                    "missed cycle",
                    Self::measurement_bool(measurement, "missed_cycle"),
                ),
            ],
        );

        ui.add_space(4.0);
        Self::render_measurement_section(
            ui,
            "\u{f140}  Deadline",
            &[
                (
                    "wake lateness",
                    Self::measurement_ns(measurement, "wake_lateness_ns"),
                ),
                ("latency", Self::measurement_ns(measurement, "latency_ns")),
                (
                    "skipped cycles",
                    Self::measurement_u64(measurement, "skipped_cycle_count"),
                ),
                (
                    "deadline missed",
                    Self::measurement_bool(measurement, "deadline_missed"),
                ),
            ],
        );

        ui.add_space(4.0);
        Self::render_measurement_section(
            ui,
            "\u{f085}  Runtime stages",
            &[
                (
                    "devices read",
                    Self::measurement_ns(measurement, "devices_read_ns"),
                ),
                (
                    "plugins",
                    Self::measurement_ns(measurement, "plugins_time_ns"),
                ),
                (
                    "devices write",
                    Self::measurement_ns(measurement, "devices_write_ns"),
                ),
            ],
        );

        ui.add_space(4.0);
        Self::render_measurement_section(
            ui,
            "\u{f0ce}  Event",
            &[
                ("sequence", Self::measurement_u64(measurement, "seq")),
                (
                    "timestamp",
                    Self::measurement_ns(measurement, "timestamp_ns"),
                ),
                (
                    "dropped events",
                    Self::measurement_u64(measurement, "dropped_event_count"),
                ),
            ],
        );
    }

    fn render_measurement_section(ui: &mut egui::Ui, title: &str, rows: &[(&str, String)]) {
        egui::CollapsingHeader::new(RichText::new(title).size(13.0).strong())
            .default_open(true)
            .show(ui, |ui| {
                ui.add_space(4.0);
                egui::Grid::new(title)
                    .num_columns(2)
                    .spacing([18.0, 6.0])
                    .show(ui, |ui| {
                        for (label, value) in rows {
                            ui.label(RichText::new(*label).color(egui::Color32::from_gray(150)));
                            ui.label(RichText::new(value).monospace());
                            ui.end_row();
                        }
                    });
            });
    }

    fn measurement_u64(measurement: &serde_json::Value, key: &str) -> String {
        measurement
            .get(key)
            .and_then(serde_json::Value::as_u64)
            .map(|value| value.to_string())
            .unwrap_or_else(|| "-".to_string())
    }

    fn measurement_ns(measurement: &serde_json::Value, key: &str) -> String {
        let Some(value) = measurement.get(key).and_then(serde_json::Value::as_u64) else {
            return "-".to_string();
        };

        if value >= 1_000_000 {
            format!("{:.3} ms", value as f64 / 1_000_000.0)
        } else if value >= 1_000 {
            format!("{:.3} us", value as f64 / 1_000.0)
        } else {
            format!("{value} ns")
        }
    }

    fn measurement_bool(measurement: &serde_json::Value, key: &str) -> String {
        measurement
            .get(key)
            .and_then(serde_json::Value::as_bool)
            .map(|value| if value { "1" } else { "0" }.to_string())
            .unwrap_or_else(|| "-".to_string())
    }
}
