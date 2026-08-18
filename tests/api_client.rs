use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::mpsc;
use std::thread;

use rtsyn_ui::api::{ApiClient, NodeKind, NodeState, ValueType};

fn capture_request(run: impl FnOnce(ApiClient)) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let (tx, rx) = mpsc::channel();
    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut buffer = [0u8; 4096];
        let size = stream.read(&mut buffer).unwrap();
        let request = String::from_utf8_lossy(&buffer[..size]).to_string();
        stream
            .write_all(
                b"HTTP/1.1 202 Accepted\r\nContent-Length: 17\r\nConnection: close\r\n\r\n{\"accepted\":true}",
            )
            .unwrap();
        tx.send(request).unwrap();
    });

    run(ApiClient::new(format!("http://127.0.0.1:{port}")));
    let request = rx.recv().unwrap();
    handle.join().unwrap();
    request
}

fn capture_request_with_response(
    run: impl FnOnce(ApiClient),
    response_body: &'static str,
) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let (tx, rx) = mpsc::channel();
    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut buffer = [0u8; 4096];
        let size = stream.read(&mut buffer).unwrap();
        let request = String::from_utf8_lossy(&buffer[..size]).to_string();
        stream
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    response_body.len(),
                    response_body
                )
                .as_bytes(),
            )
            .unwrap();
        tx.send(request).unwrap();
    });

    run(ApiClient::new(format!("http://127.0.0.1:{port}")));
    let request = rx.recv().unwrap();
    handle.join().unwrap();
    request
}

#[test]
fn load_plugin_posts_expected_request() {
    let request = capture_request(|client| {
        let response = client
            .load_node(NodeKind::Plugin, "/tmp/plugin.so")
            .unwrap();
        assert_eq!(response.status, 202);
    });

    assert!(request.starts_with("POST /commands/plugin/load HTTP/1.1"));
    assert!(request.contains("\"module_path\":\"/tmp/plugin.so\""));
}

#[test]
fn node_command_route_probe_gets_capabilities() {
    let request = capture_request_with_response(
        |client| {
            assert!(client.node_command_routes_available().unwrap());
        },
        "{\"commands\":{\"plugin\":true,\"runtime_period\":true}}",
    );

    assert!(request.starts_with("GET /capabilities HTTP/1.1"));
}

#[test]
fn node_command_route_probe_rejects_old_capabilities() {
    let request = capture_request_with_response(
        |client| {
            assert!(!client.node_command_routes_available().unwrap());
        },
        "{\"commands\":{\"plugin\":true}}",
    );

    assert!(request.starts_with("GET /capabilities HTTP/1.1"));
}

#[test]
fn telemetry_values_file_gets_path() {
    let request = capture_request_with_response(
        |client| {
            let path = client.telemetry_values_file().unwrap();
            assert_eq!(path, "/tmp/rtsyn-values");
        },
        "{\"path\":\"/tmp/rtsyn-values\"}",
    );

    assert!(request.starts_with("GET /telemetry/values-file HTTP/1.1"));
}

#[test]
fn measurements_gets_latest_metrics() {
    let request = capture_request_with_response(
        |client| {
            let response = client.measurements().unwrap();
            assert_eq!(response.status, 200);
            assert!(response.body.contains("\"latency_ns\":20"));
        },
        "{\"available\":true,\"latency_ns\":20}",
    );

    assert!(request.starts_with("GET /measurements HTTP/1.1"));
}

#[test]
fn set_runtime_period_posts_expected_request() {
    let request = capture_request(|client| {
        let response = client.set_runtime_period(500000).unwrap();
        assert_eq!(response.status, 202);
    });

    assert!(request.starts_with("POST /commands/runtime/period HTTP/1.1"));
    assert!(request.contains("\"period_ns\":500000"));
}

#[test]
fn load_device_posts_expected_request() {
    let request = capture_request(|client| {
        let response = client
            .load_node(NodeKind::Device, "/tmp/device.so")
            .unwrap();
        assert_eq!(response.status, 202);
    });

    assert!(request.starts_with("POST /commands/device/load HTTP/1.1"));
    assert!(request.contains("\"module_path\":\"/tmp/device.so\""));
}

#[test]
fn set_param_posts_typed_value() {
    let request = capture_request(|client| {
        let response = client.set_param(7, 2, ValueType::F64, "3.5").unwrap();
        assert_eq!(response.status, 202);
    });

    assert!(request.starts_with("POST /commands/param HTTP/1.1"));
    assert!(request.contains("\"node_id\":7"));
    assert!(request.contains("\"param_id\":2"));
    assert!(request.contains("\"value_type\":\"f64\""));
    assert!(request.contains("\"value\":3.5"));
}

#[test]
fn transition_node_posts_runtime_state_code() {
    let request = capture_request(|client| {
        let response = client.transition_node(4, NodeState::Restart).unwrap();
        assert_eq!(response.status, 202);
    });

    assert!(request.starts_with("POST /commands/plugin HTTP/1.1"));
    assert!(request.contains("\"plugin_id\":4"));
    assert!(request.contains("\"plugin_state\":3"));
}

#[test]
fn add_connection_posts_expected_request() {
    let request = capture_request(|client| {
        let response = client.add_connection(5, 1, 2, 3, 4).unwrap();
        assert_eq!(response.status, 202);
    });

    assert!(request.starts_with("POST /commands/connection/add HTTP/1.1"));
    assert!(request.contains("\"connection_id\":5"));
    assert!(request.contains("\"source_node_id\":1"));
    assert!(request.contains("\"source_port_id\":2"));
    assert!(request.contains("\"destination_node_id\":3"));
    assert!(request.contains("\"destination_port_id\":4"));
}

#[test]
fn remove_connection_posts_expected_request() {
    let request = capture_request(|client| {
        let response = client.remove_connection(5).unwrap();
        assert_eq!(response.status, 202);
    });

    assert!(request.starts_with("POST /commands/connection/remove HTTP/1.1"));
    assert!(request.contains("\"connection_id\":5"));
}
