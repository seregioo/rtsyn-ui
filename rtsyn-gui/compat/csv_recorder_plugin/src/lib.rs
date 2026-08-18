use rtsyn_plugin::ui::{DisplaySchema, ExtendableInputs, PluginBehavior};
use rtsyn_plugin::{Plugin, Port};

pub struct CsvRecorderedPlugin {
    _id: u64,
}

impl CsvRecorderedPlugin {
    pub fn new(id: u64) -> Self {
        Self { _id: id }
    }
}

impl Plugin for CsvRecorderedPlugin {
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
