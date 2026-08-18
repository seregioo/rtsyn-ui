use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::mpsc;
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

use rtsyn_ui::api::{GlobalCommand, NodeKind, ValueType};
use rtsyn_ui::cli::{execute, CliCommand, CliOptions};

fn capture_execute_requests(run: impl FnOnce(rtsyn_ui::api::ApiClient)) -> Vec<String> {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let (tx, rx) = mpsc::channel();
    let handle = thread::spawn(move || {
        let mut requests = Vec::new();
        for response in [
            b"HTTP/1.1 200 OK\r\nContent-Length: 11\r\nConnection: close\r\n\r\n{\"ok\":true}"
                .as_slice(),
            b"HTTP/1.1 202 Accepted\r\nContent-Length: 17\r\nConnection: close\r\n\r\n{\"accepted\":true}"
                .as_slice(),
        ] {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buffer = [0u8; 4096];
            let size = stream.read(&mut buffer).unwrap();
            requests.push(String::from_utf8_lossy(&buffer[..size]).to_string());
            stream.write_all(response).unwrap();
        }
        tx.send(requests).unwrap();
    });

    run(rtsyn_ui::api::ApiClient::new(format!(
        "http://127.0.0.1:{port}"
    )));
    let requests = rx.recv().unwrap();
    handle.join().unwrap();
    requests
}

fn unique_temp_dir(name: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!("rtsyn-cli-{name}-{nanos}"))
}

#[test]
fn parses_api_override_and_plugin_load() {
    let options = CliOptions::parse([
        "--api".to_string(),
        "http://127.0.0.1:17191".to_string(),
        "plugin".to_string(),
        "load".to_string(),
        "/tmp/plugin.so".to_string(),
    ])
    .unwrap();

    assert_eq!(options.api_base_url, "http://127.0.0.1:17191");
    assert_eq!(
        options.command,
        CliCommand::LoadNode {
            kind: NodeKind::Plugin,
            path: "/tmp/plugin.so".to_string()
        }
    );
}

#[test]
fn execute_plugin_load_checks_daemon_and_posts_load_request() {
    let root = unique_temp_dir("module");
    let release = root
        .join("build")
        .join("linux")
        .join("x86_64")
        .join("release");
    std::fs::create_dir_all(&release).unwrap();
    std::fs::write(root.join("xmake.lua"), "").unwrap();
    let shared_library = release.join("librtsyn-adder.so");
    std::fs::write(&shared_library, "").unwrap();

    let requests = capture_execute_requests(|client| {
        let output = execute(
            &client,
            CliCommand::LoadNode {
                kind: NodeKind::Plugin,
                path: root.join("xmake.lua").to_string_lossy().to_string(),
            },
        )
        .unwrap();
        assert!(output.contains("accepted"));
    });

    assert_eq!(requests.len(), 2);
    assert!(requests[0].starts_with("GET /health HTTP/1.1"));
    assert!(requests[1].starts_with("POST /commands/plugin/load HTTP/1.1"));
    assert!(requests[1].contains(&format!(
        "\"module_path\":\"{}\"",
        shared_library.to_string_lossy()
    )));

    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn parses_daemon_commands() {
    let start = CliOptions::parse(["daemon".to_string(), "start".to_string()]).unwrap();
    assert_eq!(start.command, CliCommand::DaemonStart);

    let stop = CliOptions::parse(["daemon".to_string(), "stop".to_string()]).unwrap();
    assert_eq!(stop.command, CliCommand::DaemonStop);

    let status = CliOptions::parse(["daemon".to_string(), "status".to_string()]).unwrap();
    assert_eq!(status.command, CliCommand::DaemonStatus);
}

#[test]
fn parses_device_add_and_engine_stop() {
    let device = CliOptions::parse([
        "device".to_string(),
        "add".to_string(),
        "test-device".to_string(),
    ])
    .unwrap();
    assert_eq!(
        device.command,
        CliCommand::AddNode {
            kind: NodeKind::Device,
            name: "test-device".to_string()
        }
    );

    let engine = CliOptions::parse(["engine".to_string(), "stop".to_string()]).unwrap();
    assert_eq!(engine.command, CliCommand::Engine(GlobalCommand::Stop));
}

#[test]
fn parses_measurements_command() {
    let metrics = CliOptions::parse(["measurements".to_string()]).unwrap();
    assert_eq!(metrics.command, CliCommand::Measurements);

    let alias = CliOptions::parse(["metrics".to_string()]).unwrap();
    assert_eq!(alias.command, CliCommand::Measurements);
}

#[test]
fn parses_connection_commands() {
    let add = CliOptions::parse([
        "connection".to_string(),
        "add".to_string(),
        "7".to_string(),
        "1".to_string(),
        "2".to_string(),
        "3".to_string(),
        "4".to_string(),
    ])
    .unwrap();
    assert_eq!(
        add.command,
        CliCommand::AddConnection {
            connection_id: 7,
            source_node_id: 1,
            source_port_id: 2,
            destination_node_id: 3,
            destination_port_id: 4
        }
    );

    let remove =
        CliOptions::parse(["connection".to_string(), "rm".to_string(), "7".to_string()]).unwrap();
    assert_eq!(
        remove.command,
        CliCommand::RemoveConnection { connection_id: 7 }
    );
}

#[test]
fn parses_subscriptions_and_param_set() {
    let subscription = CliOptions::parse([
        "subscribe".to_string(),
        "states".to_string(),
        "9".to_string(),
        "on".to_string(),
        "0x10".to_string(),
    ])
    .unwrap();
    assert_eq!(
        subscription.command,
        CliCommand::SubscribeStates {
            node_id: 9,
            send: true,
            mask: 16
        }
    );

    let param = CliOptions::parse([
        "param".to_string(),
        "set".to_string(),
        "3".to_string(),
        "1".to_string(),
        "string".to_string(),
        "hello".to_string(),
    ])
    .unwrap();
    assert_eq!(
        param.command,
        CliCommand::SetParam {
            node_id: 3,
            param_id: 1,
            value_type: ValueType::String,
            value: "hello".to_string()
        }
    );
}

#[test]
fn api_command_reports_stopped_daemon_before_dispatch() {
    let client = rtsyn_ui::api::ApiClient::new("http://127.0.0.1:1");
    let error = execute(&client, CliCommand::Health).unwrap_err();

    assert!(error
        .to_string()
        .contains("RTSyn daemon is not running; start it with `daemon start`"));
}
