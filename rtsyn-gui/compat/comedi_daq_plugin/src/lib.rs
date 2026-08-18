use rtsyn_plugin::ui::DisplaySchema;
use rtsyn_plugin::{Plugin, Port};

pub struct ComediDaqPlugin {
    _id: u64,
}

impl ComediDaqPlugin {
    pub fn new(id: u64) -> Self {
        Self { _id: id }
    }
}

impl Plugin for ComediDaqPlugin {
    fn inputs(&self) -> Vec<Port> {
        Vec::new()
    }

    fn outputs(&self) -> Vec<Port> {
        vec![Port::new("out")]
    }

    fn display_schema(&self) -> Option<DisplaySchema> {
        Some(DisplaySchema {
            inputs: Vec::new(),
            outputs: vec!["out".to_string()],
            variables: Vec::new(),
        })
    }
}
