use eframe::egui;
use eframe::egui::RichText;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc;

use crate::gui::runtime_bridge::LogicMessage;
use crate::gui::state::*;
use crate::gui::utils::{distance_to_segment, format_f64_6, truncate_f64};
use crate::gui::workspace_model::{
    prune_extendable_inputs_plugin_connections, ConnectionDefinition,
};
use crate::gui::{GuiApp, WorkspaceSettingsDraft};
use std::time::{Duration, Instant};

mod connections;
mod plotters;
mod plugins;
mod widgets;
mod workspaces;

pub use widgets::{kv_row_wrapped, styled_button};

pub const BUTTON_SIZE: egui::Vec2 = egui::vec2(100.0, 26.0);
