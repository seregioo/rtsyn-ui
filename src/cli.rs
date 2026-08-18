use std::path::PathBuf;

use crate::api::{parse_node_state, ApiClient, GlobalCommand, NodeKind, ValueType};
use crate::daemon::{DaemonController, DaemonStatus};
use crate::module::build_runtime_module;
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
    Health,
    Measurements,
    Engine(GlobalCommand),
    LoadNode {
        kind: NodeKind,
        path: String,
    },
    AddNode {
        kind: NodeKind,
        name: String,
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
        | CliCommand::WorkspaceNew { .. } => {
            unreachable!("daemon commands are handled before API dispatch")
        }
        CliCommand::Health => client.health()?,
        CliCommand::Measurements => client.measurements()?,
        CliCommand::Engine(command) => client.global_command(command)?,
        CliCommand::LoadNode { kind, path } => {
            let build = build_runtime_module(&path)?;
            client.load_node(kind, &build.shared_library.to_string_lossy())?
        }
        CliCommand::AddNode { kind, name } => client.add_node(kind, &name)?,
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
        let mut args = args.into_iter();
        let mut api_base_url = DEFAULT_API_BASE_URL.to_string();
        let mut positional = Vec::new();

        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--api" => {
                    api_base_url = args
                        .next()
                        .ok_or_else(|| Error::Parse("--api requires a URL".to_string()))?;
                }
                "-h" | "--help" => return Err(Error::Parse(help_text())),
                _ if arg.starts_with('-') => {
                    return Err(Error::Parse(format!("unknown option `{arg}`")));
                }
                _ => positional.push(arg),
            }
        }

        let command = parse_command(&positional)?;
        Ok(Self {
            api_base_url,
            command,
        })
    }
}

fn parse_command(args: &[String]) -> Result<CliCommand> {
    let Some(command) = args.first().map(String::as_str) else {
        return Err(Error::Parse(help_text()));
    };

    match command {
        "daemon" => parse_daemon_command(args),
        "health" => expect_len(args, 1).map(|_| CliCommand::Health),
        "measurements" | "metrics" => expect_len(args, 1).map(|_| CliCommand::Measurements),
        "engine" => {
            expect_len(args, 2)?;
            let command = match args[1].as_str() {
                "stop" => GlobalCommand::Stop,
                "pause" => GlobalCommand::Pause,
                "resume" => GlobalCommand::Resume,
                other => return Err(Error::Parse(format!("unknown engine command `{other}`"))),
            };
            Ok(CliCommand::Engine(command))
        }
        "plugin" => parse_node_command(NodeKind::Plugin, args),
        "device" => parse_node_command(NodeKind::Device, args),
        "connection" => parse_connection_command(args),
        "subscribe" => parse_subscribe_command(args),
        "param" => parse_param_command(args),
        "workspace" => parse_workspace_command(args),
        _ => Err(Error::Parse(format!("unknown command `{command}`"))),
    }
}

fn parse_daemon_command(args: &[String]) -> Result<CliCommand> {
    expect_len(args, 2)?;
    match args[1].as_str() {
        "start" => Ok(CliCommand::DaemonStart),
        "stop" => Ok(CliCommand::DaemonStop),
        "status" => Ok(CliCommand::DaemonStatus),
        other => Err(Error::Parse(format!("unknown daemon command `{other}`"))),
    }
}

fn parse_node_command(kind: NodeKind, args: &[String]) -> Result<CliCommand> {
    if args.len() < 3 {
        return Err(Error::Parse(
            "node command needs an action and value".to_string(),
        ));
    }
    match args[1].as_str() {
        "load" => expect_len(args, 3).map(|_| CliCommand::LoadNode {
            kind,
            path: args[2].clone(),
        }),
        "add" => expect_len(args, 3).map(|_| CliCommand::AddNode {
            kind,
            name: args[2].clone(),
        }),
        "start" => {
            expect_len(args, 3)?;
            Ok(CliCommand::TransitionNode {
                node_id: parse_u32(&args[2])?,
                state: "start".to_string(),
            })
        }
        "stop" => {
            expect_len(args, 3)?;
            Ok(CliCommand::TransitionNode {
                node_id: parse_u32(&args[2])?,
                state: "stop".to_string(),
            })
        }
        "restart" => {
            expect_len(args, 3)?;
            Ok(CliCommand::RestartNode {
                node_id: parse_u32(&args[2])?,
            })
        }
        other => Err(Error::Parse(format!("unknown node action `{other}`"))),
    }
}

fn parse_connection_command(args: &[String]) -> Result<CliCommand> {
    match args.get(1).map(String::as_str) {
        Some("add") => {
            expect_len(args, 7)?;
            Ok(CliCommand::AddConnection {
                connection_id: parse_u32(&args[2])?,
                source_node_id: parse_u32(&args[3])?,
                source_port_id: parse_u32(&args[4])?,
                destination_node_id: parse_u32(&args[5])?,
                destination_port_id: parse_u32(&args[6])?,
            })
        }
        Some("rm" | "remove") => {
            expect_len(args, 3)?;
            Ok(CliCommand::RemoveConnection {
                connection_id: parse_u32(&args[2])?,
            })
        }
        Some(other) => Err(Error::Parse(format!("unknown connection action `{other}`"))),
        None => Err(Error::Parse(
            "connection command needs an action".to_string(),
        )),
    }
}

fn parse_subscribe_command(args: &[String]) -> Result<CliCommand> {
    expect_len(args, 5)?;
    let node_id = parse_u32(&args[2])?;
    let send = parse_send(&args[3])?;
    let mask = parse_u64(&args[4])?;
    match args[1].as_str() {
        "ports" => Ok(CliCommand::SubscribePorts {
            node_id,
            send,
            mask,
        }),
        "states" => Ok(CliCommand::SubscribeStates {
            node_id,
            send,
            mask,
        }),
        other => Err(Error::Parse(format!("unknown subscription `{other}`"))),
    }
}

fn parse_param_command(args: &[String]) -> Result<CliCommand> {
    if args.len() != 6 || args[1] != "set" {
        return Err(Error::Parse(
            "usage: param set <node-id> <param-id> <type> <value>".to_string(),
        ));
    }
    Ok(CliCommand::SetParam {
        node_id: parse_u32(&args[2])?,
        param_id: parse_u32(&args[3])?,
        value_type: ValueType::parse(&args[4])?,
        value: args[5].clone(),
    })
}

fn parse_workspace_command(args: &[String]) -> Result<CliCommand> {
    match args.get(1).map(String::as_str) {
        Some("new") => {
            expect_len(args, 4)?;
            Ok(CliCommand::WorkspaceNew {
                path: PathBuf::from(&args[2]),
                name: args[3].clone(),
            })
        }
        Some("apply") => {
            expect_len(args, 3)?;
            Ok(CliCommand::WorkspaceApply {
                path: PathBuf::from(&args[2]),
            })
        }
        Some(other) => Err(Error::Parse(format!("unknown workspace action `{other}`"))),
        None => Err(Error::Parse(
            "workspace command needs an action".to_string(),
        )),
    }
}

fn expect_len(args: &[String], len: usize) -> Result<()> {
    if args.len() == len {
        Ok(())
    } else {
        Err(Error::Parse(format!(
            "expected {len} arguments, got {}",
            args.len()
        )))
    }
}

fn parse_u32(value: &str) -> Result<u32> {
    value
        .parse()
        .map_err(|_| Error::Parse(format!("expected u32, got `{value}`")))
}

fn parse_u64(value: &str) -> Result<u64> {
    if let Some(hex) = value.strip_prefix("0x") {
        u64::from_str_radix(hex, 16)
    } else {
        value.parse()
    }
    .map_err(|_| Error::Parse(format!("expected u64, got `{value}`")))
}

fn parse_send(value: &str) -> Result<bool> {
    match value {
        "on" | "true" | "1" => Ok(true),
        "off" | "false" | "0" => Ok(false),
        _ => Err(Error::Parse(format!("expected on/off, got `{value}`"))),
    }
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
    "usage: rtsyn-cli [--api URL] <command>\n\
     commands:\n\
       daemon start|stop|status\n\
       health\n\
       measurements\n\
       engine stop|pause|resume\n\
       plugin load <path> | plugin add <name> | plugin start|stop|restart <id>\n\
       device load <path> | device add <name> | device start|stop|restart <id>\n\
       connection add <id> <src-node> <src-port> <dst-node> <dst-port> | connection rm <id>\n\
       subscribe ports|states <node-id> on|off <mask>\n\
       param set <node-id> <param-id> f32|f64|i64|u64|string <value>\n\
       workspace new <path> <name> | workspace apply <path>"
        .to_string()
}
