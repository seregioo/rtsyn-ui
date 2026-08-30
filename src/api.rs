use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

use crate::{Error, Result, DEFAULT_API_BASE_URL};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApiClient {
    base_url: String,
    timeout: Duration,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApiResponse {
    pub status: u16,
    pub body: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApiEndpoint {
    pub host: String,
    pub port: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GlobalCommand {
    Stop,
    Pause,
    Resume,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NodeKind {
    Plugin,
    Device,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NodeState {
    Init,
    Start,
    Process,
    Restart,
    Stop,
    Fini,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ValueType {
    F32,
    F64,
    I64,
    U64,
    String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CsvValueSelector {
    pub node_id: u32,
    pub value_id: u32,
    pub kind: CsvValueKind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CsvValueKind {
    Port,
    State,
}

impl Default for ApiClient {
    fn default() -> Self {
        Self::new(DEFAULT_API_BASE_URL)
    }
}

impl ApiClient {
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into().trim_end_matches('/').to_string(),
            timeout: Duration::from_secs(2),
        }
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub fn endpoint(&self) -> Result<ApiEndpoint> {
        ApiEndpoint::parse(&self.base_url)
    }

    pub fn health(&self) -> Result<ApiResponse> {
        self.request("GET", "/health", "")
    }

    pub fn node_command_routes_available(&self) -> Result<bool> {
        let response = self.request("GET", "/capabilities", "")?;
        Ok(response.status != 404 && capabilities_support_daemon_routes(&response.body))
    }

    pub fn csv_measurements_available(&self) -> Result<bool> {
        let response = self.request("GET", "/capabilities", "")?;
        Ok(response.status != 404 && response.body.contains("\"csv_measurements\":true"))
    }

    pub fn telemetry_events(&self) -> Result<ApiResponse> {
        self.request("GET", "/telemetry/events", "")
    }

    pub fn telemetry_values_file(&self) -> Result<String> {
        let response = self.request("GET", "/telemetry/values-file", "")?;
        if !(200..300).contains(&response.status) {
            return Err(Error::Api(format!(
                "telemetry values file request failed with HTTP {}",
                response.status
            )));
        }
        extract_json_string_field(&response.body, "path")
            .ok_or_else(|| Error::Parse("telemetry values file response has no path".to_string()))
    }

    pub fn measurements(&self) -> Result<ApiResponse> {
        self.request("GET", "/measurements", "")
    }

    pub fn runtime_nodes(&self) -> Result<ApiResponse> {
        self.request("GET", "/runtime/nodes", "")
    }

    pub fn stop_engine(&self) -> Result<ApiResponse> {
        self.global_command(GlobalCommand::Stop)
    }

    pub fn global_command(&self, command: GlobalCommand) -> Result<ApiResponse> {
        let command = match command {
            GlobalCommand::Stop => "stop",
            GlobalCommand::Pause => "pause",
            GlobalCommand::Resume => "resume",
        };
        self.request(
            "POST",
            "/commands/global",
            &format!("{{\"command\":\"{command}\"}}"),
        )
    }

    pub fn load_node(&self, kind: NodeKind, module_path: &str) -> Result<ApiResponse> {
        let endpoint = match kind {
            NodeKind::Plugin => "/commands/plugin/load",
            NodeKind::Device => "/commands/device/load",
        };
        self.clone().with_timeout(Duration::from_secs(25)).request(
            "POST",
            endpoint,
            &format!("{{\"module_path\":\"{}\"}}", escape_json(module_path)),
        )
    }

    pub fn add_node(&self, kind: NodeKind, node_name: &str) -> Result<ApiResponse> {
        let endpoint = match kind {
            NodeKind::Plugin => "/commands/plugin/add",
            NodeKind::Device => "/commands/device/add",
        };
        self.request(
            "POST",
            endpoint,
            &format!("{{\"node_name\":\"{}\"}}", escape_json(node_name)),
        )
    }

    pub fn remove_node(&self, node_id: u32) -> Result<ApiResponse> {
        self.request(
            "POST",
            "/commands/node/remove",
            &format!("{{\"node_id\":{node_id}}}"),
        )
    }

    pub fn add_connection(
        &self,
        connection_id: u32,
        source_node_id: u32,
        source_port_id: u32,
        destination_node_id: u32,
        destination_port_id: u32,
    ) -> Result<ApiResponse> {
        self.request(
            "POST",
            "/commands/connection/add",
            &format!(
                "{{\"connection_id\":{connection_id},\"source_node_id\":{source_node_id},\"source_port_id\":{source_port_id},\"destination_node_id\":{destination_node_id},\"destination_port_id\":{destination_port_id}}}"
            ),
        )
    }

    pub fn remove_connection(&self, connection_id: u32) -> Result<ApiResponse> {
        self.request(
            "POST",
            "/commands/connection/remove",
            &format!("{{\"connection_id\":{connection_id}}}"),
        )
    }

    pub fn transition_node(&self, node_id: u32, state: NodeState) -> Result<ApiResponse> {
        self.request(
            "POST",
            "/commands/plugin",
            &format!(
                "{{\"plugin_id\":{node_id},\"plugin_state\":{}}}",
                node_state_code(state)
            ),
        )
    }

    pub fn subscribe_port_values(
        &self,
        node_id: u32,
        send: bool,
        mask: u64,
    ) -> Result<ApiResponse> {
        self.request(
            "POST",
            "/commands/port-values",
            &format!(
                "{{\"plugin_id\":{node_id},\"send\":{},\"portsyn_mask\":{mask}}}",
                bool_json(send)
            ),
        )
    }

    pub fn subscribe_node_states(
        &self,
        node_id: u32,
        send: bool,
        mask: u64,
    ) -> Result<ApiResponse> {
        self.request(
            "POST",
            "/commands/variables",
            &format!(
                "{{\"plugin_id\":{node_id},\"send\":{},\"variable_mask\":{mask}}}",
                bool_json(send)
            ),
        )
    }

    pub fn set_param(
        &self,
        node_id: u32,
        param_id: u32,
        value_type: ValueType,
        value: &str,
    ) -> Result<ApiResponse> {
        let value_type_name = value_type.name();
        let rendered_value = match value_type {
            ValueType::String => format!("\"{}\"", escape_json(value)),
            ValueType::F32 | ValueType::F64 | ValueType::I64 | ValueType::U64 => value.to_string(),
        };
        self.request(
            "POST",
            "/commands/param",
            &format!(
                "{{\"node_id\":{node_id},\"param_id\":{param_id},\"value_type\":\"{value_type_name}\",\"value\":{rendered_value}}}"
            ),
        )
    }

    pub fn set_runtime_period(&self, period_ns: u64) -> Result<ApiResponse> {
        self.request(
            "POST",
            "/commands/runtime/period",
            &format!("{{\"period_ns\":{period_ns}}}"),
        )
    }

    pub fn set_runtime_priority(&self, priority: i32) -> Result<ApiResponse> {
        self.request(
            "POST",
            "/commands/runtime/priority",
            &format!("{{\"priority\":{priority}}}"),
        )
    }

    pub fn set_runtime_deadline_tolerance(&self, tolerance_ns: u64) -> Result<ApiResponse> {
        self.request(
            "POST",
            "/commands/runtime/deadline-tolerance",
            &format!("{{\"tolerance_ns\":{tolerance_ns}}}"),
        )
    }

    pub fn configure_csv_values_file(
        &self,
        path: &str,
        names: &[String],
        value_ids: &[u32],
    ) -> Result<ApiResponse> {
        let values = value_ids
            .iter()
            .map(|value_id| CsvValueSelector {
                node_id: u32::MAX,
                value_id: *value_id,
                kind: CsvValueKind::Port,
            })
            .collect::<Vec<_>>();
        self.configure_csv_telemetry_file(path, names, &values, &[])
    }

    pub fn configure_csv_telemetry_file(
        &self,
        path: &str,
        names: &[String],
        values: &[CsvValueSelector],
        measurement_fields: &[String],
    ) -> Result<ApiResponse> {
        let names = names
            .iter()
            .map(|name| format!("\"{}\"", escape_json(name)))
            .collect::<Vec<_>>()
            .join(",");
        let values = values
            .iter()
            .map(|value| {
                format!(
                    "{{\"node_id\":{},\"value_id\":{},\"kind\":\"{}\"}}",
                    value.node_id,
                    value.value_id,
                    match &value.kind {
                        CsvValueKind::Port => "port",
                        CsvValueKind::State => "state",
                    }
                )
            })
            .collect::<Vec<_>>()
            .join(",");
        let measurement_fields = measurement_fields
            .iter()
            .map(|field| format!("\"{}\"", escape_json(field)))
            .collect::<Vec<_>>()
            .join(",");
        self.request(
            "POST",
            "/telemetry/csv-file",
            &format!(
                "{{\"path\":\"{}\",\"names\":[{names}],\"values\":[{values}],\"measurement_fields\":[{measurement_fields}]}}",
                escape_json(path)
            ),
        )
    }

    pub fn stop_csv_telemetry_file(&self) -> Result<ApiResponse> {
        self.request("POST", "/telemetry/csv-file", "{\"enabled\":false}")
    }

    fn request(&self, method: &str, path: &str, body: &str) -> Result<ApiResponse> {
        let endpoint = self.endpoint()?;
        let mut stream = TcpStream::connect((endpoint.host.as_str(), endpoint.port))?;
        stream.set_read_timeout(Some(self.timeout))?;
        stream.set_write_timeout(Some(self.timeout))?;

        let request = format!(
            "{method} {path} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            endpoint.host,
            body.len(),
            body
        );
        stream.write_all(request.as_bytes())?;

        let mut response = String::new();
        stream.read_to_string(&mut response)?;
        parse_http_response(&response)
    }
}

impl ValueType {
    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "f32" => Ok(Self::F32),
            "f64" => Ok(Self::F64),
            "i64" => Ok(Self::I64),
            "u64" => Ok(Self::U64),
            "string" => Ok(Self::String),
            _ => Err(Error::Parse(format!("unsupported value type `{value}`"))),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::F32 => "f32",
            Self::F64 => "f64",
            Self::I64 => "i64",
            Self::U64 => "u64",
            Self::String => "string",
        }
    }
}

pub fn node_state_code(state: NodeState) -> u8 {
    match state {
        NodeState::Init => 0,
        NodeState::Start => 1,
        NodeState::Process => 2,
        NodeState::Restart => 3,
        NodeState::Stop => 4,
        NodeState::Fini => 5,
    }
}

pub fn parse_node_state(value: &str) -> Result<NodeState> {
    match value {
        "init" => Ok(NodeState::Init),
        "start" | "started" => Ok(NodeState::Start),
        "process" | "run" | "running" => Ok(NodeState::Process),
        "restart" | "restarted" => Ok(NodeState::Restart),
        "stop" | "stopped" => Ok(NodeState::Stop),
        "fini" | "finish" | "finished" => Ok(NodeState::Fini),
        _ => Err(Error::Parse(format!("unsupported node state `{value}`"))),
    }
}

fn bool_json(value: bool) -> &'static str {
    if value {
        "true"
    } else {
        "false"
    }
}

fn parse_http_response(response: &str) -> Result<ApiResponse> {
    let (head, body) = response.split_once("\r\n\r\n").unwrap_or((response, ""));
    let status_line = head
        .lines()
        .next()
        .ok_or_else(|| Error::Api("empty HTTP response".to_string()))?;
    let status = status_line
        .split_whitespace()
        .nth(1)
        .ok_or_else(|| Error::Api(format!("invalid HTTP status line `{status_line}`")))?
        .parse::<u16>()
        .map_err(|_| Error::Api(format!("invalid HTTP status line `{status_line}`")))?;
    Ok(ApiResponse {
        status,
        body: body.to_string(),
    })
}

impl ApiEndpoint {
    pub fn parse(base_url: &str) -> Result<Self> {
        let stripped = base_url
            .strip_prefix("http://")
            .ok_or_else(|| Error::Parse("only http:// API URLs are supported".to_string()))?;
        let authority = stripped.split('/').next().unwrap_or(stripped);
        let (host, port) = match authority.rsplit_once(':') {
            Some((host, port)) => {
                let port = port
                    .parse::<u16>()
                    .map_err(|_| Error::Parse(format!("invalid API port `{port}`")))?;
                (host.to_string(), port)
            }
            None => (authority.to_string(), 80),
        };
        if host.is_empty() {
            return Err(Error::Parse("API host is required".to_string()));
        }
        Ok(Self { host, port })
    }
}

fn capabilities_support_daemon_routes(body: &str) -> bool {
    let compact = body
        .chars()
        .filter(|character| !character.is_ascii_whitespace())
        .collect::<String>();
    compact.contains("\"runtime_period\":true")
        && compact.contains("\"runtime_priority\":true")
        && compact.contains("\"runtime_deadline_tolerance\":true")
        && compact.contains("\"csv_measurements\":true")
        && compact.contains("\"runtime\":{\"nodes\":true}")
}

pub fn escape_json(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '\\' => output.push_str("\\\\"),
            '"' => output.push_str("\\\""),
            '\n' => output.push_str("\\n"),
            '\r' => output.push_str("\\r"),
            '\t' => output.push_str("\\t"),
            _ => output.push(ch),
        }
    }
    output
}

fn extract_json_string_field(body: &str, field: &str) -> Option<String> {
    let needle = format!("\"{field}\":\"");
    let start = body.find(&needle)? + needle.len();
    let mut output = String::new();
    let mut escaped = false;
    for ch in body[start..].chars() {
        if escaped {
            match ch {
                '"' => output.push('"'),
                '\\' => output.push('\\'),
                'n' => output.push('\n'),
                'r' => output.push('\r'),
                't' => output.push('\t'),
                other => output.push(other),
            }
            escaped = false;
            continue;
        }
        match ch {
            '\\' => escaped = true,
            '"' => return Some(output),
            other => output.push(other),
        }
    }
    None
}
