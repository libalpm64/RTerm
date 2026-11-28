use super::*;
use crate::agent::{Action, Bridge, OutputStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Instant;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

struct Client {
    socket: BufReader<TcpStream>,
    address: SocketAddr,
}

impl Client {
    async fn connect(address: SocketAddr) -> Self {
        assert!(address.ip().is_loopback());
        let socket = TcpStream::connect(address).await.unwrap();
        socket.set_nodelay(true).unwrap();
        Self {
            socket: BufReader::new(socket),
            address,
        }
    }

    async fn request(
        &mut self,
        method: &str,
        params: Value,
        id: usize,
        close: bool,
    ) -> (u16, Value) {
        let body = json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}).to_string();
        let connection = if close { "close" } else { "keep-alive" };
        let request = format!(
            "POST /mcp HTTP/1.1\r\nHost: {}\r\n{TOKEN_HEADER}: fixture-stress-token\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: {connection}\r\n\r\n{body}",
            self.address,
            body.len()
        );
        tokio::time::timeout(Duration::from_secs(10), async {
            self.socket.write_all(request.as_bytes()).await.unwrap();
            let mut line = String::new();
            self.socket.read_line(&mut line).await.unwrap();
            let status: u16 = line.split_whitespace().nth(1).unwrap().parse().unwrap();
            let mut length = 0;
            loop {
                line.clear();
                assert!(self.socket.read_line(&mut line).await.unwrap() > 0);
                if line == "\r\n" {
                    break;
                }
                if let Some((key, value)) = line.split_once(':')
                    && key.eq_ignore_ascii_case("content-length")
                {
                    length = value.trim().parse().unwrap();
                }
            }
            let mut bytes = vec![0; length];
            self.socket.read_exact(&mut bytes).await.unwrap();
            let value = if bytes.is_empty() {
                Value::Null
            } else {
                serde_json::from_slice(&bytes).unwrap()
            };
            if status == 200 {
                assert_eq!(value["id"], id);
            }
            (status, value)
        })
        .await
        .expect("local HTTP request timed out")
    }

    async fn tool(&mut self, name: &str, arguments: Value, id: usize) -> Value {
        let (status, value) = self
            .request(
                "tools/call",
                json!({"name":name,"arguments":arguments}),
                id,
                false,
            )
            .await;
        assert_eq!(status, 200, "{value}");
        value["result"]["structuredContent"].clone()
    }
}

struct Fixture {
    address: SocketAddr,
    access: McpAccess,
    stop: Arc<AtomicBool>,
    executed: Arc<AtomicUsize>,
    held: Arc<AtomicUsize>,
    discovery_delay: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
    server: tokio::task::JoinHandle<()>,
}

impl Fixture {
    async fn start() -> Self {
        let bridge = Bridge::start(&eframe::egui::Context::default()).unwrap();
        let access = bridge.mcp_access();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let service = router(access.clone(), address, "fixture-stress-token");
        let server = tokio::spawn(async move {
            axum::serve(listener, service).await.unwrap();
        });
        let stop = Arc::new(AtomicBool::new(false));
        let executed = Arc::new(AtomicUsize::new(0));
        let held = Arc::new(AtomicUsize::new(0));
        let discovery_delay = Arc::new(AtomicBool::new(false));
        let delay = discovery_delay.clone();
        let (done, count, hold_count) = (stop.clone(), executed.clone(), held.clone());
        let worker = std::thread::spawn(move || {
            let mut pending = Vec::new();
            while !done.load(Ordering::Acquire) {
                match bridge.requests.recv_timeout(Duration::from_millis(1)) {
                    Ok(request) => match &request.action {
                        Action::ListSessions => {
                            if delay.load(Ordering::Acquire) {
                                std::thread::sleep(Duration::from_millis(20));
                            }
                            request.reply.send(json!({"ok":true,"sessions":[{"session_id":1,"name":"fixture"}],"saved_sessions":[],"tabs":[]})).unwrap();
                        }
                        Action::Execute { command, .. } => {
                            count.fetch_add(1, Ordering::Relaxed);
                            assert!(
                                request
                                    .reply
                                    .start(Instant::now() + Duration::from_secs(30))
                            );
                            if command == "hold" {
                                hold_count.fetch_add(1, Ordering::Release);
                                pending.push(request);
                            } else {
                                if command == "output" {
                                    request.reply.capture_output(
                                        OutputStream::Stdout,
                                        &vec![b'x'; 256 * 1024],
                                    );
                                    request
                                        .reply
                                        .capture_output(OutputStream::Stderr, b"fixture stderr");
                                }
                                request
                                    .reply
                                    .send(json!({"ok":true,"exit_code":0,"command_success":true}))
                                    .unwrap();
                            }
                        }
                        _ => {
                            request
                                .reply
                                .send(crate::agent::error("unsupported fixture action"))
                                .unwrap();
                        }
                    },
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                }
                pending.retain(|request| request.authorized());
            }
        });
        Self {
            address,
            access,
            stop,
            executed,
            held,
            discovery_delay,
            worker: Some(worker),
            server,
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.access.stop();
        self.stop.store(true, Ordering::Release);
        self.server.abort();
        if let Some(worker) = self.worker.take() {
            worker.join().unwrap();
        }
    }
}

fn runtime() -> tokio::runtime::Runtime {
    // Match the production tunnel's single async worker and 8 blocking workers.
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(8)
        .build()
        .unwrap()
}

fn report(label: &str, mut samples: Vec<f64>, elapsed: Duration, rejected: usize) {
    samples.sort_by(f64::total_cmp);
    let n = samples.len();
    let at = |q: f64| samples[((n - 1) as f64 * q).round() as usize];
    println!(
        "MCP_STRESS {label} n={n} rejected={rejected} throughput={:.1}/s p50={:.3}ms p95={:.3}ms p99={:.3}ms max={:.3}ms",
        (n + rejected) as f64 / elapsed.as_secs_f64(),
        at(0.5),
        at(0.95),
        at(0.99),
        at(1.0)
    );
}

async fn traffic(
    address: SocketAddr,
    label: &str,
    clients: usize,
    per_client: usize,
    method: &'static str,
    params: Value,
    churn: bool,
) {
    let start = Instant::now();
    let mut tasks = tokio::task::JoinSet::new();
    for lane in 0..clients {
        let params = params.clone();
        tasks.spawn(async move {
            let mut client = Client::connect(address).await;
            let mut samples = Vec::new();
            let mut rejected = 0;
            for i in 0..per_client {
                if churn && i > 0 {
                    client = Client::connect(address).await;
                }
                let start = Instant::now();
                let (status, value) = client
                    .request(method, params.clone(), lane * per_client + i, churn)
                    .await;
                if status == 429 {
                    rejected += 1;
                    continue;
                }
                assert_eq!(status, 200);
                assert!(value.get("error").is_none(), "{value}");
                assert_ne!(value["result"]["isError"], true, "{value}");
                samples.push(start.elapsed().as_secs_f64() * 1000.0);
            }
            (samples, rejected)
        });
    }
    let mut samples = Vec::new();
    let mut rejected = 0;
    while let Some(result) = tasks.join_next().await {
        let (values, errors) = result.unwrap();
        samples.extend(values);
        rejected += errors;
    }
    assert!(!samples.is_empty());
    if clients <= 4 {
        assert_eq!(rejected, 0);
    }
    report(label, samples, start.elapsed(), rejected);
}

async fn long_poll_probe(fixture: &Fixture) -> (bool, Duration) {
    let mut client = Client::connect(fixture.address).await;
    let mut pollers = tokio::task::JoinSet::new();
    for i in 0..4 {
        let op = format!("hold-{i}");
        assert_eq!(
            client
                .tool(
                    "execute",
                    json!({"session_id":i+1,"operation_id":op,"command":"hold","wait_ms":0}),
                    i
                )
                .await["ok"],
            true
        );
        let address = fixture.address;
        pollers.spawn(async move {
            let mut c = Client::connect(address).await;
            c.tool("poll", json!({"operation_id":op,"wait_ms":5000}), i)
                .await
        });
    }
    let deadline = Instant::now() + Duration::from_secs(2);
    while fixture.held.load(Ordering::Acquire) < 4 {
        assert!(Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    // All four blocking wait slots occupied before the short discovery call.
    tokio::time::sleep(Duration::from_millis(100)).await;
    fixture.discovery_delay.store(true, Ordering::Release);
    let start = Instant::now();
    let listed = client.tool("list_sessions", json!({}), 50).await;
    let success = listed["ok"] == true;
    let latency = start.elapsed();
    fixture.discovery_delay.store(false, Ordering::Release);
    for i in 0..4 {
        assert_eq!(
            client
                .tool(
                    "cancel",
                    json!({"operation_id":format!("hold-{i}")}),
                    60 + i
                )
                .await["status"],
            "cancelled"
        );
    }
    while let Some(result) = pollers.join_next().await {
        assert_eq!(result.unwrap()["status"], "cancelled");
    }
    (success, latency)
}

#[test]
#[ignore = "local MCP throughput and connection churn benchmark"]
fn local_mcp_stress() {
    runtime().block_on(async {
        let fixture = Fixture::start().await;
        let address = fixture.address;
        traffic(address,"ping-sequential",1,1000,"ping",json!({}),false).await;
        traffic(address,"tools-list-sequential",1,1000,"tools/list",json!({}),false).await;
        traffic(address,"tools-list-concurrent",4,500,"tools/list",json!({}),false).await;
        traffic(address,"connection-churn",4,500,"ping",json!({}),true).await;
        traffic(address,"discovery",4,250,"tools/call",json!({"name":"list_sessions","arguments":{}}),false).await;
        traffic(address,"overload",32,100,"tools/list",json!({}),false).await;
        let mut client = Client::connect(address).await;
        let start = Instant::now();
        let mut samples = Vec::new();
        for i in 0..1000 {
            let before = Instant::now();
            let op = format!("job-{i}");
            let result = client.tool("execute",json!({"session_id":1,"operation_id":op,"command":"fixture","wait_ms":1000}),i).await;
            assert_eq!(result["status"],"completed");
            assert_eq!(result["exit_code"],0);
            samples.push(before.elapsed().as_secs_f64()*1000.0);
        }
        assert_eq!(fixture.executed.load(Ordering::Acquire),1000);
        report("submit-complete",samples,start.elapsed(),0);
        assert_eq!(client.tool("history",json!({"limit":32}),0).await["operations"].as_array().unwrap().len(),32);
        let result = client.tool("execute",json!({"session_id":1,"operation_id":"large-output","command":"output","wait_ms":1000}),0).await;
        assert_eq!(result["status"],"completed");
        let start=Instant::now();
        let mut samples=Vec::new();
        for i in 0..400 {
            let before=Instant::now();
            let page=client.tool("poll",json!({"operation_id":"large-output","since":(i%4)*65536,"wait_ms":0}),i).await;
            assert_eq!(page["stdout"].as_str().unwrap().len(),65536);
            assert!(page.get("output_path").is_none());
            samples.push(before.elapsed().as_secs_f64()*1000.0);
        }
        report("output-page-64k",samples,start.elapsed(),0);
        let (success,latency)=long_poll_probe(&fixture).await;
        println!("MCP_STRESS discovery-with-four-polls success={success} latency_ms={:.3}",latency.as_secs_f64()*1000.0);
        fixture.access.stop();
        assert_eq!(client.request("ping",json!({}),0,false).await.0,503);
    });
}

#[test]
fn discovery_remains_available_during_four_long_polls() {
    runtime().block_on(async {
        let fixture = Fixture::start().await;
        let (success, latency) = long_poll_probe(&fixture).await;
        assert!(success, "discovery failed with four long polls");
        assert!(latency < Duration::from_secs(1), "{latency:?}");
    });
}

#[cfg(target_os = "linux")]
pub(crate) fn resource_sample(label: &str) -> (usize, usize, usize) {
    let status = std::fs::read_to_string("/proc/self/status").unwrap();
    let number = |key: &str| {
        status
            .lines()
            .find_map(|line| line.strip_prefix(key))
            .unwrap()
            .split_whitespace()
            .next()
            .unwrap()
            .parse::<usize>()
            .unwrap()
    };
    let fds = std::fs::read_dir("/proc/self/fd").unwrap().count();
    let threads = number("Threads:");
    let rss = number("VmRSS:");
    let anon = number("RssAnon:");
    println!("MCP_RESOURCE {label} fds={fds} threads={threads} rss_kib={rss} anon_kib={anon}");
    (fds, threads, anon)
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "local idle TCP connection resource stress"]
fn idle_connection_resource_stress() {
    runtime().block_on(async {
        let fixture = Fixture::start().await;
        let mut c = Client::connect(fixture.address).await;
        assert_eq!(c.request("ping", json!({}), 0, false).await.0, 200);
        drop(c);
        tokio::time::sleep(Duration::from_millis(100)).await;
        let before = resource_sample("idle-before");
        let mut sockets = Vec::new();
        for _ in 0..256 {
            sockets.push(Client::connect(fixture.address).await);
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
        let peak = resource_sample("idle-256-connections");
        println!(
            "MCP_RESOURCE idle_server_fd_delta={}",
            peak.0.saturating_sub(before.0 + 256)
        );
        drop(sockets);
        tokio::time::sleep(Duration::from_millis(250)).await;
        let recovered = resource_sample("idle-recovered");
        assert!(recovered.0 <= before.0 + 2, "socket leak");
        let mut c = Client::connect(fixture.address).await;
        assert_eq!(c.request("ping", json!({}), 1, false).await.0, 200);
    });
}
