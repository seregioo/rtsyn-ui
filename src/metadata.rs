use std::fs;
use std::path::Path;

use crate::api::ValueType;
use crate::{Error, Result};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PluginUiMetadata {
    pub name: String,
    pub description: String,
    pub controls: Vec<PluginControl>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ParseSection {
    Root,
    Control,
    Ignored,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginControl {
    pub name: String,
    pub label: String,
    pub kind: ControlKind,
    pub target: ControlTarget,
    pub param_id: Option<u32>,
    pub value_type: ValueType,
    pub default_value: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ControlKind {
    Number,
    Text,
    Toggle,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ControlTarget {
    Param,
}

impl PluginUiMetadata {
    pub fn read_from(path: &Path) -> Result<Self> {
        Self::parse(&fs::read_to_string(path)?)
    }

    pub fn parse(contents: &str) -> Result<Self> {
        let mut metadata = PluginUiMetadata::default();
        let mut current_control: Option<PluginControl> = None;
        let mut section = ParseSection::Root;

        for (line_index, raw_line) in contents.lines().enumerate() {
            let line_number = line_index + 1;
            let line = raw_line.split('#').next().unwrap_or(raw_line).trim();
            if line.is_empty() {
                continue;
            }
            if line == "[[controls]]" {
                if let Some(control) = current_control.take() {
                    metadata.controls.push(control);
                }
                current_control = Some(PluginControl::default());
                section = ParseSection::Control;
                continue;
            }
            if line.starts_with("[[") && line.ends_with("]]") {
                if let Some(control) = current_control.take() {
                    metadata.controls.push(control);
                }
                section = ParseSection::Ignored;
                continue;
            }

            let (key, value) = line
                .split_once('=')
                .map(|(key, value)| (key.trim(), parse_string(value.trim())))
                .ok_or_else(|| {
                    Error::Parse(format!("line {line_number}: expected `key = value`"))
                })?;
            match section {
                ParseSection::Control => match current_control.as_mut() {
                    Some(control) => match key {
                    "name" => control.name = value,
                    "label" => control.label = value,
                    "kind" => control.kind = parse_control_kind(&value, line_number)?,
                    "target" => control.target = parse_control_target(&value, line_number)?,
                    "param_id" => control.param_id = Some(parse_u32(&value, line_number)?),
                    "value_type" => control.value_type = ValueType::parse(&value)?,
                    "default" => control.default_value = value,
                    _ => return Err(unknown_key(key, line_number)),
                },
                    None => return Err(Error::Parse(format!(
                        "line {line_number}: control section without active control"
                    ))),
                },
                ParseSection::Root => match key {
                    "name" => metadata.name = value,
                    "description" => metadata.description = value,
                    _ => return Err(unknown_key(key, line_number)),
                },
                ParseSection::Ignored => {}
            }
        }

        if let Some(control) = current_control.take() {
            metadata.controls.push(control);
        }
        metadata.validate()?;
        Ok(metadata)
    }

    fn validate(&self) -> Result<()> {
        if self.name.trim().is_empty() {
            return Err(Error::Parse(
                "plugin UI metadata name is required".to_string(),
            ));
        }
        for control in &self.controls {
            if control.name.trim().is_empty() {
                return Err(Error::Parse("control name is required".to_string()));
            }
            if control.label.trim().is_empty() {
                return Err(Error::Parse(format!(
                    "control `{}` label is required",
                    control.name
                )));
            }
            if control.target == ControlTarget::Param && control.param_id.is_none() {
                return Err(Error::Parse(format!(
                    "control `{}` needs param_id",
                    control.name
                )));
            }
        }
        Ok(())
    }
}

impl Default for PluginControl {
    fn default() -> Self {
        Self {
            name: String::new(),
            label: String::new(),
            kind: ControlKind::Number,
            target: ControlTarget::Param,
            param_id: None,
            value_type: ValueType::F64,
            default_value: String::new(),
        }
    }
}

fn parse_string(value: &str) -> String {
    value.trim().trim_matches('"').to_string()
}

fn parse_u32(value: &str, line_number: usize) -> Result<u32> {
    value
        .parse()
        .map_err(|_| Error::Parse(format!("line {line_number}: expected u32")))
}

fn parse_control_kind(value: &str, line_number: usize) -> Result<ControlKind> {
    match value {
        "number" => Ok(ControlKind::Number),
        "text" => Ok(ControlKind::Text),
        "toggle" => Ok(ControlKind::Toggle),
        _ => Err(Error::Parse(format!(
            "line {line_number}: invalid control kind"
        ))),
    }
}

fn parse_control_target(value: &str, line_number: usize) -> Result<ControlTarget> {
    match value {
        "param" => Ok(ControlTarget::Param),
        _ => Err(Error::Parse(format!(
            "line {line_number}: invalid control target"
        ))),
    }
}

fn unknown_key(key: &str, line_number: usize) -> Error {
    Error::Parse(format!("line {line_number}: unknown key `{key}`"))
}

#[cfg(test)]
mod tests {
    use super::PluginUiMetadata;

    #[test]
    fn comedi_ui_metadata_is_valid() {
        let contents = include_str!("../../rtsyn-module-device-comedi/rtsyn-node-ui.toml");
        let metadata = PluginUiMetadata::parse(contents).expect("valid COMEDI UI metadata");

        assert_eq!(metadata.name, "COMEDI");
        assert_eq!(metadata.controls.len(), 3);
    }
}
