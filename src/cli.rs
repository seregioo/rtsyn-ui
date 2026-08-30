use std::path::PathBuf;

use clap::{Args, CommandFactory, Parser, Subcommand};

use crate::api::{parse_node_state, ApiClient, GlobalCommand, NodeKind, ValueType};
use crate::daemon::{DaemonController, DaemonStatus};
use crate::workspace::Workspace;
use crate::{Error, Result, DEFAULT_API_BASE_URL};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CliOptions {
    pub api_base_url: String,
    pub command: CliCommand,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CliCommand {
    DaemonStart,
    DaemonStop,
    DaemonStatus,
    DaemonRun,
    Health,
    Nodes,
    Measurements,
    ConfigureCsvTelemetry {
        path: String,
        names: Vec<String>,
        value_ids: Vec<u32>,
    },
    SetRuntimePeriod {
        period_ns: u64,
    },
    SetRuntimePriority {
        priority: i32,
    },
    SetRuntimeDeadlineTolerance {
        tolerance_ns: u64,
    },
    Engine(GlobalCommand),
    LoadNode {
        kind: NodeKind,
        path: String,
    },
    AddNode {
        kind: NodeKind,
        name: String,
    },
    RemoveNode {
        node_id: u32,
    },
    AddConnection {
        connection_id: u32,
        source_node_id: u32,
        source_port_id: u32,
        destination_node_id: u32,
        destination_port_id: u32,
    },
    RemoveConnection {
        connection_id: u32,
    },
    TransitionNode {
        node_id: u32,
        state: String,
    },
    RestartNode {
        node_id: u32,
    },
    SubscribePorts {
        node_id: u32,
        send: bool,
        mask: u64,
    },
    SubscribeStates {
        node_id: u32,
        send: bool,
        mask: u64,
    },
    SetParam {
        node_id: u32,
        param_id: u32,
        value_type: ValueType,
        value: String,
    },
    WorkspaceNew {
        path: PathBuf,
        name: String,
    },
    WorkspaceApply {
        path: PathBuf,
    },
}

pub fn run<I>(args: I) -> Result<String>
where
    I: IntoIterator<Item = String>,
{
    let options = CliOptions::parse(args)?;
    execute(&ApiClient::new(&options.api_base_url), options.command)
}

pub fn execute(client: &ApiClient, command: CliCommand) -> Result<String> {
    match &command {
        CliCommand::DaemonStart => {
            return DaemonController::default_for_api(client.base_url()).start();
        }
        CliCommand::DaemonStop => {
            return DaemonController::default_for_api(client.base_url()).stop();
        }
        CliCommand::DaemonStatus => {
            let status = DaemonController::default_for_api(client.base_url()).status();
            return Ok(match status {
                DaemonStatus::Running => "daemon running".to_string(),
                DaemonStatus::Stopped => "daemon stopped".to_string(),
            });
        }
        CliCommand::DaemonRun => {
            crate::daemon::run_foreground(client.base_url())?;
            return Ok("daemon stopped".to_string());
        }
        CliCommand::WorkspaceNew { path, name } => {
            Workspace::new(name.clone()).write_to(path)?;
            return Ok(format!("created workspace `{}`", path.display()));
        }
        _ => {}
    }

    ensure_daemon_running(client)?;

    let response = match command {
        CliCommand::DaemonStart
        | CliCommand::DaemonStop
        | CliCommand::DaemonStatus
        | CliCommand::DaemonRun
        | CliCommand::WorkspaceNew { .. } => {
            unreachable!("daemon commands are handled before API dispatch")
        }
        CliCommand::Health => client.health()?,
        CliCommand::Nodes => client.runtime_nodes()?,
        CliCommand::Measurements => client.measurements()?,
        CliCommand::ConfigureCsvTelemetry {
            path,
            names,
            value_ids,
        } => client.configure_csv_values_file(&path, &names, &value_ids)?,
        CliCommand::SetRuntimePeriod { period_ns } => client.set_runtime_period(period_ns)?,
        CliCommand::SetRuntimePriority { priority } => client.set_runtime_priority(priority)?,
        CliCommand::SetRuntimeDeadlineTolerance { tolerance_ns } => {
            client.set_runtime_deadline_tolerance(tolerance_ns)?
        }
        CliCommand::Engine(command) => client.global_command(command)?,
        CliCommand::LoadNode { kind, path } => client.load_node(kind, &path)?,
        CliCommand::AddNode { kind, name } => client.add_node(kind, &name)?,
        CliCommand::RemoveNode { node_id } => client.remove_node(node_id)?,
        CliCommand::AddConnection {
            connection_id,
            source_node_id,
            source_port_id,
            destination_node_id,
            destination_port_id,
        } => client.add_connection(
            connection_id,
            source_node_id,
            source_port_id,
            destination_node_id,
            destination_port_id,
        )?,
        CliCommand::RemoveConnection { connection_id } => {
            client.remove_connection(connection_id)?
        }
        CliCommand::TransitionNode { node_id, state } => {
            client.transition_node(node_id, parse_node_state(&state)?)?
        }
        CliCommand::RestartNode { node_id } => {
            client.transition_node(node_id, parse_node_state("restart")?)?
        }
        CliCommand::SubscribePorts {
            node_id,
            send,
            mask,
        } => client.subscribe_port_values(node_id, send, mask)?,
        CliCommand::SubscribeStates {
            node_id,
            send,
            mask,
        } => client.subscribe_node_states(node_id, send, mask)?,
        CliCommand::SetParam {
            node_id,
            param_id,
            value_type,
            value,
        } => client.set_param(node_id, param_id, value_type, &value)?,
        CliCommand::WorkspaceApply { path } => {
            Workspace::read_from(&path)?.apply(client)?;
            return Ok(format!("applied workspace `{}`", path.display()));
        }
    };

    ensure_success(response.status, &response.body)?;
    Ok(response.body)
}

impl CliOptions {
    pub fn parse<I>(args: I) -> Result<Self>
    where
        I: IntoIterator<Item = String>,
    {
        let raw = args.into_iter().collect::<Vec<_>>();
        let mut argv = Vec::with_capacity(raw.len() + 1);
        argv.push("rtsyn-cli".to_string());
        argv.extend(raw);

        let parsed = ClapCli::try_parse_from(argv)
            .map_err(|error| Error::Parse(error.render().to_string()))?;
        Ok(Self {
            api_base_url: parsed.api,
            command: parsed.command.try_into()?,
        })
    }
}

#[derive(Parser, Debug)]
#[command(
    name = "rtsyn-cli",
    about = "Control an RTSyn daemon through the HTTP API",
    disable_version_flag = true
)]
struct ClapCli {
    #[arg(long, default_value = DEFAULT_API_BASE_URL, help = "RTSyn API base URL")]
    api: String,
    #[command(subcommand)]
    command: ClapCommand,
}

#[derive(Subcommand, Debug)]
enum ClapCommand {
    #[command(about = "Start, stop, or inspect the local RTSyn daemon")]
    Daemon {
        #[command(subcommand)]
        command: DaemonCommand,
    },
    #[command(about = "Request API health information")]
    Health,
    #[command(about = "List runtime nodes currently known by the daemon")]
    Nodes,
    #[command(about = "Read the latest runtime timing measurements")]
    Measurements,
    #[command(about = "Alias for measurements")]
    Metrics,
    #[command(about = "Configure telemetry export")]
    Telemetry {
        #[command(subcommand)]
        command: TelemetryCommand,
    },
    #[command(about = "Send global engine commands")]
    Engine {
        #[command(subcommand)]
        command: EngineCommand,
    },
    #[command(about = "Configure runtime execution settings")]
    Runtime {
        #[command(subcommand)]
        command: RuntimeCommand,
    },
    #[command(about = "Load, add, or transition plugin nodes")]
    Plugin(NodeCommand),
    #[command(about = "Load, add, or transition device nodes")]
    Device(NodeCommand),
    #[command(about = "Add or remove runtime connections")]
    Connection {
        #[command(subcommand)]
        command: ConnectionCommand,
    },
    #[command(about = "Subscribe or unsubscribe telemetry values")]
    Subscribe {
        #[command(subcommand)]
        command: SubscribeCommand,
    },
    #[command(about = "Set runtime node parameters")]
    Param {
        #[command(subcommand)]
        command: ParamCommand,
    },
    #[command(about = "Create or apply client-side workspaces")]
    Workspace {
        #[command(subcommand)]
        command: WorkspaceCommand,
    },
}

#[derive(Subcommand, Debug)]
enum DaemonCommand {
    #[command(about = "Start the RTSyn daemon process")]
    Start,
    #[command(about = "Stop the RTSyn daemon process")]
    Stop,
    #[command(about = "Print whether the daemon is running")]
    Status,
    #[command(hide = true)]
    Run,
}

#[derive(Subcommand, Debug)]
enum TelemetryCommand {
    #[command(about = "Write selected telemetry values to a CSV file")]
    Csv {
        #[arg(help = "CSV output path")]
        path: String,
        #[arg(
            required = true,
            value_parser = parse_csv_column,
            help = "Column selector in name:value-id form"
        )]
        columns: Vec<CsvColumnArg>,
    },
}

#[derive(Subcommand, Debug)]
enum EngineCommand {
    #[command(about = "Request engine shutdown")]
    Stop,
    #[command(about = "Pause runtime execution")]
    Pause,
    #[command(about = "Resume runtime execution")]
    Resume,
}

#[derive(Subcommand, Debug)]
enum RuntimeCommand {
    #[command(about = "Set the runtime cycle period in nanoseconds")]
    Period {
        #[arg(help = "Cycle period in nanoseconds")]
        period_ns: u64,
    },
    #[command(about = "Set the realtime thread priority")]
    Priority {
        #[arg(value_parser = parse_priority, help = "Thread priority in the 0..99 range")]
        priority: i32,
    },
    #[command(about = "Set deadline tolerance for runtime measurements")]
    DeadlineTolerance {
        #[arg(help = "Allowed wake lateness in nanoseconds before deadline_missed becomes true")]
        tolerance_ns: u64,
    },
}

#[derive(Args, Debug)]
struct NodeCommand {
    #[command(subcommand)]
    command: NodeAction,
}

#[derive(Subcommand, Debug)]
enum NodeAction {
    #[command(about = "Build a module root and load its descriptor into the engine")]
    Load {
        #[arg(help = "Module root directory or xmake.lua path")]
        path: String,
    },
    #[command(about = "Add a runtime node from a loaded descriptor")]
    Add {
        #[arg(help = "Loaded descriptor name")]
        name: String,
    },
    #[command(about = "Start a runtime node")]
    Start {
        #[arg(help = "Runtime node id")]
        node_id: u32,
    },
    #[command(about = "Stop a runtime node")]
    Stop {
        #[arg(help = "Runtime node id")]
        node_id: u32,
    },
    #[command(about = "Restart a runtime node")]
    Restart {
        #[arg(help = "Runtime node id")]
        node_id: u32,
    },
    #[command(about = "Remove a runtime node")]
    Remove {
        #[arg(help = "Runtime node id")]
        node_id: u32,
    },
}

#[derive(Subcommand, Debug)]
enum ConnectionCommand {
    #[command(about = "Connect an output port to an input port")]
    Add {
        #[arg(help = "Connection id")]
        connection_id: u32,
        #[arg(help = "Source node id")]
        source_node_id: u32,
        #[arg(help = "Source port id")]
        source_port_id: u32,
        #[arg(help = "Destination node id")]
        destination_node_id: u32,
        #[arg(help = "Destination port id")]
        destination_port_id: u32,
    },
    #[command(alias = "rm", about = "Remove a runtime connection")]
    Remove {
        #[arg(help = "Connection id")]
        connection_id: u32,
    },
}

#[derive(Subcommand, Debug)]
enum SubscribeCommand {
    #[command(about = "Subscribe or unsubscribe node port values")]
    Ports(SubscribeArgs),
    #[command(about = "Subscribe or unsubscribe node state values")]
    States(SubscribeArgs),
}

#[derive(Args, Debug)]
struct SubscribeArgs {
    #[arg(help = "Runtime node id")]
    node_id: u32,
    #[arg(help = "Use on/off to enable or disable publishing")]
    send: String,
    #[arg(value_parser = parse_u64, help = "Bit mask of values to publish")]
    mask: u64,
}

#[derive(Subcommand, Debug)]
enum ParamCommand {
    #[command(about = "Set a runtime node parameter")]
    Set {
        #[arg(help = "Runtime node id")]
        node_id: u32,
        #[arg(help = "Parameter id")]
        param_id: u32,
        #[arg(value_parser = parse_value_type, help = "Value type: f32, f64, i64, u64, string")]
        value_type: ValueType,
        #[arg(help = "Parameter value")]
        value: String,
    },
}

#[derive(Subcommand, Debug)]
enum WorkspaceCommand {
    #[command(about = "Create an empty workspace file")]
    New {
        #[arg(help = "Workspace TOML path")]
        path: PathBuf,
        #[arg(help = "Workspace name")]
        name: String,
    },
    #[command(about = "Apply a workspace to the running daemon")]
    Apply {
        #[arg(help = "Workspace TOML path")]
        path: PathBuf,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct CsvColumnArg {
    name: String,
    value_id: u32,
}

impl TryFrom<ClapCommand> for CliCommand {
    type Error = Error;

    fn try_from(command: ClapCommand) -> Result<Self> {
        Ok(match command {
            ClapCommand::Daemon { command } => match command {
                DaemonCommand::Start => CliCommand::DaemonStart,
                DaemonCommand::Stop => CliCommand::DaemonStop,
                DaemonCommand::Status => CliCommand::DaemonStatus,
                DaemonCommand::Run => CliCommand::DaemonRun,
            },
            ClapCommand::Health => CliCommand::Health,
            ClapCommand::Nodes => CliCommand::Nodes,
            ClapCommand::Measurements | ClapCommand::Metrics => CliCommand::Measurements,
            ClapCommand::Telemetry {
                command: TelemetryCommand::Csv { path, columns },
            } => CliCommand::ConfigureCsvTelemetry {
                path,
                names: columns.iter().map(|column| column.name.clone()).collect(),
                value_ids: columns.iter().map(|column| column.value_id).collect(),
            },
            ClapCommand::Engine { command } => CliCommand::Engine(match command {
                EngineCommand::Stop => GlobalCommand::Stop,
                EngineCommand::Pause => GlobalCommand::Pause,
                EngineCommand::Resume => GlobalCommand::Resume,
            }),
            ClapCommand::Runtime { command } => match command {
                RuntimeCommand::Period { period_ns } => CliCommand::SetRuntimePeriod { period_ns },
                RuntimeCommand::Priority { priority } => {
                    CliCommand::SetRuntimePriority { priority }
                }
                RuntimeCommand::DeadlineTolerance { tolerance_ns } => {
                    CliCommand::SetRuntimeDeadlineTolerance { tolerance_ns }
                }
            },
            ClapCommand::Plugin(command) => node_command(NodeKind::Plugin, command.command),
            ClapCommand::Device(command) => node_command(NodeKind::Device, command.command),
            ClapCommand::Connection { command } => match command {
                ConnectionCommand::Add {
                    connection_id,
                    source_node_id,
                    source_port_id,
                    destination_node_id,
                    destination_port_id,
                } => CliCommand::AddConnection {
                    connection_id,
                    source_node_id,
                    source_port_id,
                    destination_node_id,
                    destination_port_id,
                },
                ConnectionCommand::Remove { connection_id } => {
                    CliCommand::RemoveConnection { connection_id }
                }
            },
            ClapCommand::Subscribe { command } => match command {
                SubscribeCommand::Ports(args) => CliCommand::SubscribePorts {
                    node_id: args.node_id,
                    send: parse_send(&args.send).map_err(Error::Parse)?,
                    mask: args.mask,
                },
                SubscribeCommand::States(args) => CliCommand::SubscribeStates {
                    node_id: args.node_id,
                    send: parse_send(&args.send).map_err(Error::Parse)?,
                    mask: args.mask,
                },
            },
            ClapCommand::Param { command } => match command {
                ParamCommand::Set {
                    node_id,
                    param_id,
                    value_type,
                    value,
                } => CliCommand::SetParam {
                    node_id,
                    param_id,
                    value_type,
                    value,
                },
            },
            ClapCommand::Workspace { command } => match command {
                WorkspaceCommand::New { path, name } => CliCommand::WorkspaceNew { path, name },
                WorkspaceCommand::Apply { path } => CliCommand::WorkspaceApply { path },
            },
        })
    }
}

fn node_command(kind: NodeKind, command: NodeAction) -> CliCommand {
    match command {
        NodeAction::Load { path } => CliCommand::LoadNode { kind, path },
        NodeAction::Add { name } => CliCommand::AddNode { kind, name },
        NodeAction::Start { node_id } => CliCommand::TransitionNode {
            node_id,
            state: "start".to_string(),
        },
        NodeAction::Stop { node_id } => CliCommand::TransitionNode {
            node_id,
            state: "stop".to_string(),
        },
        NodeAction::Restart { node_id } => CliCommand::RestartNode { node_id },
        NodeAction::Remove { node_id } => CliCommand::RemoveNode { node_id },
    }
}

fn parse_csv_column(value: &str) -> std::result::Result<CsvColumnArg, String> {
    let Some((name, id)) = value.split_once(':') else {
        return Err(format!(
            "expected telemetry column as name:value-id, got `{value}`"
        ));
    };
    if name.is_empty() {
        return Err("telemetry column name is empty".to_string());
    }
    Ok(CsvColumnArg {
        name: name.to_string(),
        value_id: parse_u32(id)?,
    })
}

fn parse_u32(value: &str) -> std::result::Result<u32, String> {
    value
        .parse()
        .map_err(|_| format!("expected u32, got `{value}`"))
}

fn parse_u64(value: &str) -> std::result::Result<u64, String> {
    if let Some(hex) = value.strip_prefix("0x") {
        u64::from_str_radix(hex, 16)
    } else {
        value.parse()
    }
    .map_err(|_| format!("expected u64, got `{value}`"))
}

fn parse_priority(value: &str) -> std::result::Result<i32, String> {
    let priority: i32 = value
        .parse()
        .map_err(|_| format!("expected priority in 0..99, got `{value}`"))?;
    if !(0..=99).contains(&priority) {
        return Err(format!("expected priority in 0..99, got `{value}`"));
    }
    Ok(priority)
}

fn parse_send(value: &str) -> std::result::Result<bool, String> {
    match value {
        "on" | "true" | "1" => Ok(true),
        "off" | "false" | "0" => Ok(false),
        _ => Err(format!("expected on/off, got `{value}`")),
    }
}

fn parse_value_type(value: &str) -> std::result::Result<ValueType, String> {
    ValueType::parse(value).map_err(|error| error.to_string())
}

fn ensure_success(status: u16, body: &str) -> Result<()> {
    if (200..300).contains(&status) {
        Ok(())
    } else {
        Err(Error::Api(format!("HTTP {status}: {body}")))
    }
}

fn ensure_daemon_running(client: &ApiClient) -> Result<()> {
    if DaemonController::default_for_api(client.base_url()).is_running() {
        Ok(())
    } else {
        Err(Error::Api(
            "RTSyn daemon is not running; start it with `daemon start`".to_string(),
        ))
    }
}

pub fn help_text() -> String {
    ClapCli::command().render_help().to_string()
}
