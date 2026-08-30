use crate::gui::builtin_tools::csv_recorder::CsvRecorderedPlugin;
use crate::gui::builtin_tools::live_plotter::LivePlotterPlugin;
use crate::gui::builtin_tools::performance_monitor::PerformanceMonitorPlugin;
use crate::gui::workspace_model::PluginDefinition;

use crate::gui::runtime_bridge::plugin_manager::{DynamicPluginInstance, RuntimePlugin};

pub fn create_plugin_instance(plugin: &PluginDefinition) -> Option<RuntimePlugin> {
    match plugin.kind.as_str() {
        "csv_recorder" => Some(RuntimePlugin::CsvRecorder(CsvRecorderedPlugin::new(
            plugin.id,
        ))),
        "live_plotter" => Some(RuntimePlugin::LivePlotter(LivePlotterPlugin::new(
            plugin.id,
        ))),
        "performance_monitor" => Some(RuntimePlugin::PerformanceMonitor(
            PerformanceMonitorPlugin::new(plugin.id),
        )),
        #[cfg(feature = "comedi")]
        "comedi_daq" => Some(RuntimePlugin::ComediDaq(
            crate::gui::builtin_tools::comedi_daq::ComediDaqPlugin::new(plugin.id),
        )),
        _ => {
            let library_path = plugin.config.get("library_path").and_then(|v| v.as_str())?;
            unsafe {
                DynamicPluginInstance::load(library_path, plugin.id).map(RuntimePlugin::Dynamic)
            }
        }
    }
}
