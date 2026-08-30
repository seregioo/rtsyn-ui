use std::fs;
use std::path::Path;

use crate::api::{ApiClient, NodeKind};
use crate::{Error, Result};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Workspace {
    pub name: String,
    pub description: String,
    pub nodes: Vec<WorkspaceNode>,
    pub connections: Vec<WorkspaceConnection>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceNode {
    pub kind: NodeKind,
    pub name: String,
    pub path: String,
    pub autostart: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceConnection {
    pub id: u32,
    pub from_node: u32,
    pub from_port: u32,
    pub to_node: u32,
    pub to_port: u32,
}

impl Workspace {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            description: String::new(),
            nodes: Vec::new(),
            connections: Vec::new(),
        }
    }

    pub fn read_from(path: &Path) -> Result<Self> {
        let contents = fs::read_to_string(path)?;
        Self::parse(&contents)
    }

    pub fn write_to(&self, path: &Path) -> Result<()> {
        fs::write(path, self.render())?;
        Ok(())
    }

    pub fn apply(&self, client: &ApiClient) -> Result<()> {
        for node in &self.nodes {
            let response = client.load_node(node.kind, &node.path)?;
            ensure_accepted("load node", &response.body, response.status)?;
            let response = client.add_node(node.kind, &node.name)?;
            ensure_accepted("add node", &response.body, response.status)?;
            if node.autostart {
                // Node IDs are assigned by the runtime today, so a workspace cannot
                // deterministically start the just-added node until the API exposes
                // an add response or topology query. The field is still persisted
                // for the GUI and future API support.
            }
        }
        for connection in &self.connections {
            let response = client.add_connection(
                connection.id,
                connection.from_node,
                connection.from_port,
                connection.to_node,
                connection.to_port,
            )?;
            ensure_accepted("add connection", &response.body, response.status)?;
        }
        Ok(())
    }

    pub fn parse(contents: &str) -> Result<Self> {
        let mut workspace = Workspace::default();
        let mut section = Section::Root;

        for (line_index, raw_line) in contents.lines().enumerate() {
            let line_number = line_index + 1;
            let line = strip_comment(raw_line).trim();
            if line.is_empty() {
                continue;
            }

            match line {
                "[[nodes]]" => {
                    workspace.nodes.push(WorkspaceNode {
                        kind: NodeKind::Plugin,
                        name: String::new(),
                        path: String::new(),
                        autostart: false,
                    });
                    section = Section::Node(workspace.nodes.len() - 1);
                    continue;
                }
                "[[connections]]" => {
                    workspace.connections.push(WorkspaceConnection {
                        id: workspace.connections.len() as u32,
                        from_node: 0,
                        from_port: 0,
                        to_node: 0,
                        to_port: 0,
                    });
                    section = Section::Connection(workspace.connections.len() - 1);
                    continue;
                }
                _ => {}
            }

            let (key, value) = parse_assignment(line, line_number)?;
            match section {
                Section::Root => match key {
                    "name" => workspace.name = parse_string(value),
                    "description" => workspace.description = parse_string(value),
                    _ => return Err(unknown_key(key, line_number)),
                },
                Section::Node(index) => {
                    let node = workspace.nodes.get_mut(index).expect("node section exists");
                    match key {
                        "kind" => node.kind = parse_node_kind(&parse_string(value), line_number)?,
                        "name" => node.name = parse_string(value),
                        "path" => node.path = parse_string(value),
                        "autostart" => node.autostart = parse_bool(value, line_number)?,
                        _ => return Err(unknown_key(key, line_number)),
                    }
                }
                Section::Connection(index) => {
                    let connection = workspace
                        .connections
                        .get_mut(index)
                        .expect("connection section exists");
                    match key {
                        "id" => connection.id = parse_u32(value, line_number)?,
                        "from_node" => connection.from_node = parse_u32(value, line_number)?,
                        "from_port" => connection.from_port = parse_u32(value, line_number)?,
                        "to_node" => connection.to_node = parse_u32(value, line_number)?,
                        "to_port" => connection.to_port = parse_u32(value, line_number)?,
                        _ => return Err(unknown_key(key, line_number)),
                    }
                }
            }
        }

        workspace.validate()?;
        Ok(workspace)
    }

    pub fn render(&self) -> String {
        let mut output = String::new();
        output.push_str(&format!("name = \"{}\"\n", escape_toml(&self.name)));
        output.push_str(&format!(
            "description = \"{}\"\n",
            escape_toml(&self.description)
        ));
        for node in &self.nodes {
            output.push_str("\n[[nodes]]\n");
            output.push_str(&format!("kind = \"{}\"\n", node_kind_name(node.kind)));
            output.push_str(&format!("name = \"{}\"\n", escape_toml(&node.name)));
            output.push_str(&format!("path = \"{}\"\n", escape_toml(&node.path)));
            output.push_str(&format!("autostart = {}\n", node.autostart));
        }
        for connection in &self.connections {
            output.push_str("\n[[connections]]\n");
            output.push_str(&format!("id = {}\n", connection.id));
            output.push_str(&format!("from_node = {}\n", connection.from_node));
            output.push_str(&format!("from_port = {}\n", connection.from_port));
            output.push_str(&format!("to_node = {}\n", connection.to_node));
            output.push_str(&format!("to_port = {}\n", connection.to_port));
        }
        output
    }

    fn validate(&self) -> Result<()> {
        if self.name.trim().is_empty() {
            return Err(Error::Parse("workspace name is required".to_string()));
        }
        for node in &self.nodes {
            if node.name.trim().is_empty() {
                return Err(Error::Parse("workspace node name is required".to_string()));
            }
            if node.path.trim().is_empty() {
                return Err(Error::Parse(format!(
                    "workspace node `{}` path is required",
                    node.name
                )));
            }
        }
        Ok(())
    }
}

#[derive(Clone, Copy)]
enum Section {
    Root,
    Node(usize),
    Connection(usize),
}

fn ensure_accepted(operation: &str, body: &str, status: u16) -> Result<()> {
    if (200..300).contains(&status) {
        Ok(())
    } else {
        Err(Error::Api(format!(
            "{operation} failed with HTTP {status}: {body}"
        )))
    }
}

fn strip_comment(line: &str) -> &str {
    line.split('#').next().unwrap_or(line)
}

fn parse_assignment(line: &str, line_number: usize) -> Result<(&str, &str)> {
    line.split_once('=')
        .map(|(key, value)| (key.trim(), value.trim()))
        .ok_or_else(|| Error::Parse(format!("line {line_number}: expected `key = value`")))
}

fn parse_string(value: &str) -> String {
    value
        .trim()
        .trim_matches('"')
        .replace("\\\"", "\"")
        .replace("\\n", "\n")
        .replace("\\t", "\t")
}

fn parse_bool(value: &str, line_number: usize) -> Result<bool> {
    match value.trim() {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err(Error::Parse(format!("line {line_number}: expected bool"))),
    }
}

fn parse_u32(value: &str, line_number: usize) -> Result<u32> {
    value
        .trim()
        .parse()
        .map_err(|_| Error::Parse(format!("line {line_number}: expected u32")))
}

fn parse_node_kind(value: &str, line_number: usize) -> Result<NodeKind> {
    match value {
        "plugin" => Ok(NodeKind::Plugin),
        "device" => Ok(NodeKind::Device),
        _ => Err(Error::Parse(format!(
            "line {line_number}: expected `plugin` or `device`"
        ))),
    }
}

fn node_kind_name(kind: NodeKind) -> &'static str {
    match kind {
        NodeKind::Plugin => "plugin",
        NodeKind::Device => "device",
    }
}

fn unknown_key(key: &str, line_number: usize) -> Error {
    Error::Parse(format!("line {line_number}: unknown key `{key}`"))
}

fn escape_toml(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\t', "\\t")
}
