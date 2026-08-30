use crate::gui::tool_api::ui::{DisplaySchema, ExtendableInputs, PluginBehavior};
use crate::gui::tool_api::{Plugin, Port};

pub struct LivePlotterPlugin {
    _id: u64,
}

impl LivePlotterPlugin {
    pub fn new(id: u64) -> Self {
        Self { _id: id }
    }
}

impl Plugin for LivePlotterPlugin {
    fn inputs(&self) -> Vec<Port> {
        vec![Port::new("in")]
    }

    fn outputs(&self) -> Vec<Port> {
        Vec::new()
    }

    fn display_schema(&self) -> Option<DisplaySchema> {
        Some(DisplaySchema {
            inputs: vec!["in".to_string()],
            outputs: Vec::new(),
            variables: Vec::new(),
        })
    }

    fn behavior(&self) -> PluginBehavior {
        PluginBehavior {
            supports_restart: false,
            extendable_inputs: ExtendableInputs::Manual,
            ..PluginBehavior::default()
        }
    }
}
