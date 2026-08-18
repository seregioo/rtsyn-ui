//! Plugin management and UI rendering functionality for RTSyn GUI.
//!
//! This module provides comprehensive plugin management capabilities including:
//! - Plugin card rendering and interaction
//! - Plugin installation, uninstallation, and management windows
//! - Plugin creation wizard with field configuration
//! - Plugin configuration dialogs
//! - Context menus and plugin operations
//!
//! The module handles both built-in app plugins (csv_recorder, live_plotter, etc.)
//! and user-installed plugins with dynamic UI schema support.

use super::*;
use crate::WindowFocus;

mod cards;
mod config;
mod state;
mod windows;
mod wizard;

impl GuiApp {
    const NEW_PLUGIN_TYPES: [&'static str; 6] = ["f64", "f32", "i64", "i32", "bool", "string"];

    /// Opens the plugin addition window.
    ///
    /// This function activates the plugin addition interface where users can
    /// browse installed plugins and add them to the current workspace. It
    /// prepares the window state for plugin selection and addition.
    ///
    /// # Side Effects
    /// - Sets `windows.plugins_open` to true
    /// - Resets selected plugin index to clear previous selections
    /// - Queues window focus for the plugin addition window
    /// - Window will be rendered in the next UI frame
    pub(crate) fn open_plugins(&mut self) {
        self.windows.plugins_open = true;
        self.windows.plugin_selected_index = None;
        self.pending_window_focus = Some(WindowFocus::Plugins);
    }
}
