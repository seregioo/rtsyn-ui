pub const RTSYN_PLUGIN_ABI_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PortId(pub String);

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Port {
    pub id: PortId,
}

impl Port {
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: PortId(id.into()),
        }
    }
}

pub trait Plugin {
    fn inputs(&self) -> Vec<Port>;
    fn outputs(&self) -> Vec<Port>;

    fn display_schema(&self) -> Option<ui::DisplaySchema> {
        None
    }

    fn ui_schema(&self) -> Option<ui::UISchema> {
        None
    }

    fn behavior(&self) -> ui::PluginBehavior {
        ui::PluginBehavior::default()
    }
}

pub mod ui {
    #[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
    pub struct DisplaySchema {
        #[serde(default)]
        pub outputs: Vec<String>,
        #[serde(default)]
        pub inputs: Vec<String>,
        #[serde(default)]
        pub variables: Vec<String>,
    }

    #[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
    pub struct UISchema {
        #[serde(default)]
        pub fields: Vec<UIField>,
    }

    impl UISchema {
        pub fn new() -> Self {
            Self { fields: Vec::new() }
        }

        pub fn field(mut self, field: UIField) -> Self {
            self.fields.push(field);
            self
        }
    }

    impl Default for UISchema {
        fn default() -> Self {
            Self::new()
        }
    }

    #[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
    pub struct UIField {
        pub key: String,
        pub name: String,
        #[serde(default)]
        pub label: String,
        #[serde(default)]
        pub description: String,
        #[serde(default)]
        pub value_type: Option<String>,
        #[serde(default)]
        pub default: Option<serde_json::Value>,
        #[serde(rename = "type")]
        pub field_type: FieldType,
    }

    impl UIField {
        pub fn new(name: impl Into<String>, field_type: FieldType) -> Self {
            let name = name.into();
            Self {
                key: name.clone(),
                name,
                label: String::new(),
                description: String::new(),
                value_type: None,
                default: None,
                field_type,
            }
        }

        pub fn label(mut self, label: impl Into<String>) -> Self {
            self.label = label.into();
            self
        }

        pub fn description(mut self, description: impl Into<String>) -> Self {
            self.description = description.into();
            self
        }

        pub fn default(mut self, default: serde_json::Value) -> Self {
            self.default = Some(default);
            self
        }
    }

    #[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
    #[serde(tag = "kind", rename_all = "snake_case")]
    pub enum FieldType {
        Integer {
            #[serde(default)]
            min: Option<i64>,
            #[serde(default)]
            max: Option<i64>,
            #[serde(default = "default_i64_step")]
            step: i64,
        },
        Float {
            #[serde(default)]
            min: Option<f64>,
            #[serde(default)]
            max: Option<f64>,
            #[serde(default = "default_f64_step")]
            step: f64,
        },
        Text {
            #[serde(default)]
            placeholder: Option<String>,
        },
        FilePath {
            #[serde(default)]
            placeholder: Option<String>,
        },
        Boolean,
        Choice {
            options: Vec<String>,
        },
        DynamicList {
            item_type: Box<FieldType>,
            #[serde(default = "default_add_label")]
            add_label: String,
        },
    }

    fn default_i64_step() -> i64 {
        1
    }

    fn default_f64_step() -> f64 {
        0.1
    }

    fn default_add_label() -> String {
        "Add".to_string()
    }

    impl Default for FieldType {
        fn default() -> Self {
            Self::Text { placeholder: None }
        }
    }

    #[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
    #[serde(rename_all = "snake_case")]
    pub enum ExtendableInputs {
        None,
        Manual,
        Auto { port_prefix: String },
    }

    impl Default for ExtendableInputs {
        fn default() -> Self {
            Self::None
        }
    }

    #[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
    pub struct ConnectionBehavior {
        #[serde(default)]
        pub dependent: bool,
        #[serde(default)]
        pub max_per_input: Option<usize>,
    }

    impl Default for ConnectionBehavior {
        fn default() -> Self {
            Self {
                dependent: false,
                max_per_input: Some(1),
            }
        }
    }

    #[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
    pub struct PluginBehavior {
        #[serde(default)]
        pub supports_start_stop: bool,
        #[serde(default)]
        pub supports_restart: bool,
        #[serde(default)]
        pub supports_apply: bool,
        #[serde(default)]
        pub loads_started: bool,
        #[serde(default)]
        pub external_window: bool,
        #[serde(default)]
        pub starts_expanded: bool,
        #[serde(default)]
        pub extendable_inputs: ExtendableInputs,
        #[serde(default)]
        pub connection: ConnectionBehavior,
        #[serde(default)]
        pub required_input_ports: Vec<String>,
        #[serde(default)]
        pub required_output_ports: Vec<String>,
        #[serde(default)]
        pub start_requires_connected_inputs: Vec<String>,
        #[serde(default)]
        pub start_requires_connected_outputs: Vec<String>,
    }

    impl Default for PluginBehavior {
        fn default() -> Self {
            Self {
                supports_start_stop: true,
                supports_restart: true,
                supports_apply: true,
                loads_started: false,
                external_window: false,
                starts_expanded: true,
                extendable_inputs: ExtendableInputs::None,
                connection: ConnectionBehavior::default(),
                required_input_ports: Vec::new(),
                required_output_ports: Vec::new(),
                start_requires_connected_inputs: Vec::new(),
                start_requires_connected_outputs: Vec::new(),
            }
        }
    }
}
