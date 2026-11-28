use super::*;
use crate::agent::OutputStream;
use std::time::Instant;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[test]
fn loopback_mcp_checks_origin_and_authenticates_discovery() {
    let context = eframe::egui::Context::default();
    let bridge = crate::agent::Bridge::start(&context).unwrap();
    let access = bridge.mcp_access();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = router(access.clone(), address, "fixture-token");
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let post = |headers: &'static str, method: &'static str, params: Value| async move {
            let body = json!({"jsonrpc":"2.0","id":1,"method":method,"params":params}).to_string();
            let mut stream = tokio::net::TcpStream::connect(address).await.unwrap();
            let request = format!("POST /mcp HTTP/1.1\r\nHost: {address}\r\n{headers}Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len());
            stream.write_all(request.as_bytes()).await.unwrap();
            let mut response = String::new();
            stream.read_to_string(&mut response).await.unwrap();
            response
        };
        assert!(post("", "tools/list", json!({})).await.starts_with("HTTP/1.1 403"));
        assert!(post("X-RTerm-Tunnel-Token: fixture-token\r\nOrigin: https://untrusted.example\r\n", "tools/list", json!({})).await.starts_with("HTTP/1.1 403"));
        let headers = "X-RTerm-Tunnel-Token: fixture-token\r\n";
        let decode = |response: String| {
            assert!(response.starts_with("HTTP/1.1 200"));
            serde_json::from_str::<Value>(response.split_once("\r\n\r\n").unwrap().1).unwrap()
        };
        let version = "2025-06-18";
        let initialized = decode(post(headers, "initialize", json!({"protocolVersion":version})).await);
        assert_eq!(initialized["result"]["protocolVersion"], version);
        let response = decode(post(headers, "tools/list", json!({})).await);
        let tools = response["result"]["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 8);
        assert_eq!(tools.iter().find(|tool| tool["name"] == "execute").unwrap()["annotations"]["readOnlyHint"], false);
        let history = decode(post(headers, "tools/call", json!({"name":"history","arguments":{}})).await);
        assert_eq!(history["result"]["structuredContent"]["operations"], json!([]));

        let call = |name, arguments| post(headers, "tools/call", json!({"name":name,"arguments":arguments}));
        let management = tools.iter().find(|tool| tool["name"] == "manage_library").unwrap();
        assert_eq!(management["inputSchema"]["properties"]["change"]["oneOf"].as_array().unwrap().len(), 6);
        let change = json!({"library_revision":"rev","operation_id":"manage-fixture","change":{"kind":"set_port","profile_id":"fixture","port":2222},"wait_ms":0});
        let queued = decode(call("manage_library", change.clone()).await);
        assert_eq!(queued["result"]["structuredContent"]["status"], "queued");
        let request = bridge.requests.try_recv().unwrap();
        assert!(matches!(request.action, crate::agent::Action::ManageLibrary { change: crate::agent::LibraryChange::SetPort { port: 2222, .. }, .. }));
        request.reply.send(json!({"ok":true,"rediscover":true})).unwrap();
        assert_eq!(decode(call("manage_library", change).await)["result"]["structuredContent"]["rediscover"], true);
        let invalid = decode(call("manage_library", json!({"library_revision":"rev","operation_id":"invalid-port","change":{"kind":"set_port","profile_id":"fixture","port":0}})).await);
        assert_eq!(invalid["result"]["isError"],true);
        assert!(bridge.requests.try_recv().is_err());
        let submit = |operation_id| call("execute", json!({"session_id":1,"operation_id":operation_id,"command":"fixture","wait_ms":0}));
        let poll = |operation_id| call("poll", json!({"operation_id":operation_id,"since":0,"wait_ms":0}));
        let queued = decode(submit("nonzero-exit").await);
        assert_eq!(queued["result"]["structuredContent"]["status"], "queued");
        let request = bridge.requests.try_recv().unwrap();
        request.reply.capture_output(OutputStream::Stdout, b"normal output");
        request.reply.capture_output(OutputStream::Stderr, b"diagnostic");
        request.reply.send(json!({"ok":true,"exit_code":17,"command_success":false})).unwrap();
        let polled = decode(poll("nonzero-exit").await);
        assert_eq!(polled["result"]["isError"], false);
        let result = &polled["result"]["structuredContent"];
        assert_eq!(result["stdout"], "normal output");
        assert_eq!(result["stderr"], "diagnostic");
        assert_eq!(result["exit_code"], 17);
        assert_eq!(result["command_success"], false);
        assert!(result.get("output_path").is_none());
        let completed = decode(submit("nonzero-exit").await);
        assert_eq!(completed["result"]["isError"], false);
        assert_eq!(completed["result"]["structuredContent"]["exit_code"], 17);

        decode(submit("cancelled-fixture").await);
        let cancelled_request = bridge.requests.try_recv().unwrap();
        let cancelled = decode(call("cancel", json!({"operation_id":"cancelled-fixture"})).await);
        assert!(!cancelled_request.authorized());
        assert_eq!(cancelled["result"]["isError"], false);
        assert_eq!(cancelled["result"]["structuredContent"]["status"], "cancelled");
        let polled = decode(poll("cancelled-fixture").await);
        assert_eq!(polled["result"]["isError"], false);
        assert_eq!(polled["result"]["structuredContent"]["status"], "cancelled");

        decode(submit("timed-out-fixture").await);
        let request = bridge.requests.try_recv().unwrap();
        assert!(request.reply.start(Instant::now() - Duration::from_millis(1)));
        request.reply.send(crate::agent::error("Backend stopped the command")).unwrap();
        let timed_out = decode(poll("timed-out-fixture").await);
        assert_eq!(timed_out["result"]["isError"], false);
        assert_eq!(timed_out["result"]["structuredContent"]["status"], "timed_out");
        let unknown = decode(poll("unknown-fixture").await);
        assert_eq!(unknown["result"]["isError"], true);
        let invalid = decode(call("poll", json!({})).await);
        assert_eq!(invalid["error"]["code"], -32602);

        access.stop();
        assert!(post(headers, "tools/list", json!({})).await.starts_with("HTTP/1.1 503"));
        server.abort();
    });
}
