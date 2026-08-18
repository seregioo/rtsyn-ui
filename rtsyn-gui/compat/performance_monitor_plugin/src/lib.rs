use rtsyn_plugin::ui::DisplaySchema;
use rtsyn_plugin::{Plugin, Port};

pub struct PerformanceMonitorPlugin {
    _id: u64,
}

impl PerformanceMonitorPlugin {
    pub fn new(id: u64) -> Self {
        Self { _id: id }
    }
}

impl Plugin for PerformanceMonitorPlugin {
    fn inputs(&self) -> Vec<Port> {
        Vec::new()
    }

    fn outputs(&self) -> Vec<Port> {
        vec![Port::new("latency_us")]
    }

    fn display_schema(&self) -> Option<DisplaySchema> {
        Some(DisplaySchema {
            inputs: Vec::new(),
            outputs: vec!["latency_us".to_string()],
            variables: vec!["max_latency_us".to_string()],
        })
    }
}
