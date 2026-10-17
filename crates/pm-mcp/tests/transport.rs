#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::print_stderr,
    clippy::needless_pass_by_value,
    reason = "test code"
)]

mod common;

use std::fmt::Write as _;
use std::io::{Read as _, Write as _};
use std::sync::Arc;

use pm_mcp::http::HttpServer;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};

/// Reads JSON-RPC lines until the response with `id` arrives.
async fn response<R: tokio::io::AsyncBufRead + Unpin>(
    lines: &mut tokio::io::Lines<R>,
    id: i64,
) -> Value {
    loop {
        let line = lines
            .next_line()
            .await
            .unwrap()
            .expect("server closed the stream");
        let v: Value = serde_json::from_str(&line).unwrap();
        if v["id"] == id {
            return v;
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stdio_round_trip_over_pipes() {
    let server = Arc::new(common::server());
    let (client, srv) = tokio::io::duplex(1 << 20);
    let (srv_r, srv_w) = tokio::io::split(srv);
    let task = tokio::spawn(pm_mcp::stdio::serve_io(server, srv_r, srv_w));
    let (cr, mut cw) = tokio::io::split(client);
    let mut lines = BufReader::new(cr).lines();
    let send = |v: Value| format!("{v}\n");

    cw.write_all(
        send(
            json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
            "protocolVersion": "2025-06-18", "capabilities": {},
            "clientInfo": {"name": "pipe-test", "version": "1"}}}),
        )
        .as_bytes(),
    )
    .await
    .unwrap();
    let init = response(&mut lines, 1).await;
    assert_eq!(init["result"]["serverInfo"]["name"], "postmortem-mcp");
    assert!(
        init["result"]["instructions"]
            .as_str()
            .unwrap()
            .contains("evidence")
    );
    assert!(init["result"]["capabilities"]["tools"].is_object());
    cw.write_all(send(json!({"jsonrpc": "2.0", "method": "notifications/initialized"})).as_bytes())
        .await
        .unwrap();

    cw.write_all(send(json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"})).as_bytes())
        .await
        .unwrap();
    let list = response(&mut lines, 2).await;
    let names: Vec<&str> = list["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    for n in pm_mcp::tools::AUTOPSY_TOOLS {
        assert!(names.contains(n), "{n}");
    }
    assert_eq!(
        list["result"]["tools"][0]["annotations"]["readOnlyHint"],
        true
    );

    cw.write_all(
        send(json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call",
                    "params": {"name": "get_case_summary", "arguments": {}}}))
        .as_bytes(),
    )
    .await
    .unwrap();
    let call = response(&mut lines, 3).await;
    assert_eq!(call["result"]["isError"], false);
    let text = call["result"]["content"][0]["text"].as_str().unwrap();
    let body: Value = serde_json::from_str(text).unwrap();
    assert_eq!(body["caseId"], "MCP demo");
    assert_eq!(body["result"]["dataSources"], 2);

    cw.write_all(
        send(json!({"jsonrpc": "2.0", "id": 4, "method": "tools/call",
                    "params": {"name": "add_tag", "arguments": {"ref": "file:1", "tagName": "Bookmark"}}}))
        .as_bytes(),
    )
    .await
    .unwrap();
    let refused = response(&mut lines, 4).await;
    assert_eq!(refused["result"]["isError"], true);

    cw.write_all(send(json!({"jsonrpc": "2.0", "id": 5, "method": "prompts/list"})).as_bytes())
        .await
        .unwrap();
    let prompts = response(&mut lines, 5).await;
    assert!(prompts["result"]["prompts"].as_array().unwrap().len() >= 4);
    cw.write_all(
        send(json!({"jsonrpc": "2.0", "id": 6, "method": "prompts/get",
                    "params": {"name": "around_time", "arguments": {"ref": "file:3"}}}))
        .as_bytes(),
    )
    .await
    .unwrap();
    let p = response(&mut lines, 6).await;
    assert!(
        p["result"]["messages"][0]["content"]["text"]
            .as_str()
            .unwrap()
            .contains("events_around")
    );

    drop(cw);
    drop(lines);
    let _ = tokio::time::timeout(std::time::Duration::from_secs(10), task).await;
}

/// One HTTP/1.1 request; returns (status, body).
fn http(
    addr: std::net::SocketAddr,
    token: Option<&str>,
    body: &str,
    session: Option<&str>,
) -> (u16, String, Option<String>) {
    let mut s = std::net::TcpStream::connect(addr).unwrap();
    s.set_read_timeout(Some(std::time::Duration::from_secs(20)))
        .unwrap();
    let mut req = format!(
        "POST /mcp HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nContent-Type: application/json\r\nAccept: application/json, text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n",
        addr.port(),
        body.len()
    );
    if let Some(t) = token {
        let _ = write!(req, "Authorization: Bearer {t}\r\n");
    }
    if let Some(sid) = session {
        let _ = write!(
            req,
            "Mcp-Session-Id: {sid}\r\nMcp-Protocol-Version: 2025-06-18\r\n"
        );
    }
    req.push_str("\r\n");
    req.push_str(body);
    s.write_all(req.as_bytes()).unwrap();
    let mut buf = Vec::new();
    // SSE responses stay open; read until the JSON-RPC response arrived.
    let mut chunk = [0u8; 8192];
    while let Ok(n) = s.read(&mut chunk) {
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
        let text = String::from_utf8_lossy(&buf);
        let complete = text.starts_with("HTTP/1.1 4")
            || ((text.contains("\"result\"") || text.contains("\"error\""))
                && (text.contains("}\n") || text.contains("}\r\n") || text.ends_with('}')));
        if complete && text.contains("\r\n\r\n") {
            break;
        }
    }
    let text = String::from_utf8_lossy(&buf).into_owned();
    let status: u16 = text
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let session = text
        .lines()
        .find(|l| l.to_ascii_lowercase().starts_with("mcp-session-id:"))
        .and_then(|l| l.split_once(':'))
        .map(|(_, v)| v.trim().to_owned());
    (status, text, session)
}

#[test]
fn http_transport_requires_the_token() {
    let server = Arc::new(common::server());
    let dir = tempfile::tempdir().unwrap();
    let token_file = dir.path().join("mcp-token");
    let h = HttpServer::start(server, 0, Some(&token_file)).unwrap();
    assert_eq!(std::fs::read_to_string(&token_file).unwrap(), h.token());
    assert!(h.url().starts_with("http://127.0.0.1:"));
    assert!(h.client_config_snippet().contains(h.token()));
    let init = json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
        "protocolVersion": "2025-06-18", "capabilities": {},
        "clientInfo": {"name": "http-test", "version": "1"}}})
    .to_string();
    let (status, _, _) = http(h.addr(), None, &init, None);
    assert_eq!(status, 401);
    let (status, _, _) = http(h.addr(), Some("wrong"), &init, None);
    assert_eq!(status, 401);
    let (status, body, session) = http(h.addr(), Some(h.token()), &init, None);
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("postmortem-mcp"), "{body}");
    let session = session.expect("session id");
    let (_, _, _) = http(
        h.addr(),
        Some(h.token()),
        &json!({"jsonrpc": "2.0", "method": "notifications/initialized"}).to_string(),
        Some(&session),
    );
    let call = json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call",
                      "params": {"name": "get_server_status", "arguments": {}}})
    .to_string();
    let (status, body, _) = http(h.addr(), Some(h.token()), &call, Some(&session));
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("caseOpen"), "{body}");
    let addr = h.addr();
    h.stop();
    assert!(!token_file.exists(), "token file removed on stop");
    assert!(std::net::TcpStream::connect(addr).is_err() || http(addr, None, &init, None).0 == 0);
}
