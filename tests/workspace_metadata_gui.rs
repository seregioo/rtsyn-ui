use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::mpsc;
use std::thread;

use rtsyn_ui::api::{ApiClient, NodeKind};
use rtsyn_ui::metadata::{ControlKind, PluginUiMetadata};
use rtsyn_ui::workspace::Workspace;

#[test]
fn parses_and_renders_workspace() {
    let workspace = Workspace::parse(include_str!("workspaces/demo.toml")).unwrap();

    assert_eq!(workspace.name, "demo");
    assert_eq!(workspace.nodes.len(), 2);
    assert_eq!(workspace.connections.len(), 1);
    assert_eq!(workspace.nodes[0].kind, NodeKind::Plugin);
    assert_eq!(workspace.nodes[1].kind, NodeKind::Device);
    assert_eq!(workspace.connections[0].id, 12);
    assert!(workspace.render().contains("[[nodes]]"));
    assert!(workspace.render().contains("[[connections]]"));
}

#[test]
fn workspace_apply_loads_nodes_then_adds_connections() {
    let workspace = Workspace::parse(include_str!("workspaces/demo.toml")).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let (tx, rx) = mpsc::channel();

    let handle = thread::spawn(move || {
        let mut requests = Vec::new();
        for _ in 0..5 {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buffer = [0u8; 4096];
            let size = stream.read(&mut buffer).unwrap();
            requests.push(String::from_utf8_lossy(&buffer[..size]).to_string());
            stream
                .write_all(
                    b"HTTP/1.1 202 Accepted\r\nContent-Length: 17\r\nConnection: close\r\n\r\n{\"accepted\":true}",
                )
                .unwrap();
        }
        tx.send(requests).unwrap();
    });

    let client = ApiClient::new(format!("http://127.0.0.1:{port}"));
    workspace.apply(&client).unwrap();
    let requests = rx.recv().unwrap();
    handle.join().unwrap();

    assert!(requests[0].starts_with("POST /commands/plugin/load HTTP/1.1"));
    assert!(requests[1].starts_with("POST /commands/plugin/add HTTP/1.1"));
    assert!(requests[2].starts_with("POST /commands/device/load HTTP/1.1"));
    assert!(requests[3].starts_with("POST /commands/device/add HTTP/1.1"));
    assert!(requests[4].starts_with("POST /commands/connection/add HTTP/1.1"));
    assert!(requests[4].contains("\"connection_id\":12"));
}

#[test]
fn parses_plugin_ui_metadata() {
    let metadata = PluginUiMetadata::parse(
        r#"
name = "Adder"
description = "Adds values"

[[controls]]
name = "gain"
label = "Gain"
kind = "number"
target = "param"
param_id = 0
value_type = "f64"
default = "1.0"
"#,
    )
    .unwrap();

    assert_eq!(metadata.name, "Adder");
    assert_eq!(metadata.controls.len(), 1);
    assert_eq!(metadata.controls[0].kind, ControlKind::Number);
    assert_eq!(metadata.controls[0].param_id, Some(0));
}

#[test]
fn parses_plugin_ui_metadata_with_ignored_indicators() {
    let metadata = PluginUiMetadata::parse(
        r#"
name = "RTHybrid Hindmarsh-Rose 1984 Neuron v2"
description = "Hindmarsh-Rose neuron."

[[controls]]
name = "x0"
label = "X0"
kind = "number"
target = "param"
param_id = 0
value_type = "f64"
default = "-0.9013747551021072"

[[indicators]]
name = "x"
label = "X"
target = "state"
state_id = 0
value_type = "f64"
"#,
    )
    .unwrap();

    assert_eq!(metadata.name, "RTHybrid Hindmarsh-Rose 1984 Neuron v2");
    assert_eq!(metadata.controls.len(), 1);
    assert_eq!(metadata.controls[0].name, "x0");
    assert_eq!(metadata.controls[0].default_value, "-0.9013747551021072");
}
