use super::*;
use russh_sftp::{
    protocol::{File, Handle, Name, Status, StatusCode},
    server,
};

struct DirectoryFixture {
    batch: usize,
}

impl server::Handler for DirectoryFixture {
    type Error = StatusCode;
    fn unimplemented(&self) -> Self::Error {
        StatusCode::OpUnsupported
    }
    async fn realpath(&mut self, id: u32, _: String) -> Result<Name, Self::Error> {
        Ok(Name {
            id,
            files: vec![File::dummy("/fixture")],
        })
    }
    async fn opendir(&mut self, id: u32, path: String) -> Result<Handle, Self::Error> {
        assert!(path == "." || path == "/fixture");
        self.batch = 0;
        Ok(Handle {
            id,
            handle: "dir".into(),
        })
    }
    async fn readdir(&mut self, id: u32, _: String) -> Result<Name, Self::Error> {
        if self.batch == 3 {
            return Err(StatusCode::Eof);
        }
        let first = self.batch * 100;
        self.batch += 1;
        Ok(Name {
            id,
            files: (first..first + 100)
                .map(|i| File::dummy(format!("file-{i:03}")))
                .collect(),
        })
    }
    async fn close(&mut self, id: u32, _: String) -> Result<Status, Self::Error> {
        Ok(Status {
            id,
            status_code: StatusCode::Ok,
            error_message: String::new(),
            language_tag: String::new(),
        })
    }
}

// Simulate a network link rather than delaying the serial SFTP handler:
// concurrently submitted packets can travel in the same latency window.
async fn delayed_link(
    mut read: impl tokio::io::AsyncRead + Unpin,
    mut write: impl tokio::io::AsyncWrite + Unpin,
    delay: Duration,
) {
    let mut buf = vec![0; 256 * 1024];
    while let Ok(mut count) = read.read(&mut buf).await {
        if count == 0 {
            break;
        }
        while count < buf.len() {
            match tokio::time::timeout(Duration::from_millis(1), read.read(&mut buf[count..])).await
            {
                Ok(Ok(n)) if n > 0 => count += n,
                _ => break,
            }
        }
        tokio::time::sleep(delay).await;
        if write.write_all(&buf[..count]).await.is_err() {
            break;
        }
    }
    let _ = write.shutdown().await;
}

#[tokio::test]
async fn listing_returns_all_batches_and_canonical_path() {
    let (client, stream) = tokio::io::duplex(1024 * 1024);
    server::run(stream, DirectoryFixture { batch: 0 }).await;
    let session = SftpSession::new(client).await.unwrap();
    let (path, entries) = list_directory(&session, ".").await.unwrap();
    let mut names: Vec<_> = entries.map(|entry| entry.file_name()).collect();
    names.sort();
    assert_eq!(path, "/fixture");
    assert_eq!(
        names,
        (0..300).map(|i| format!("file-{i:03}")).collect::<Vec<_>>()
    );
    session.close().await.unwrap();
}

#[test]
fn rejects_unsafe_download_names() {
    for name in [
        "",
        ".",
        "..",
        "../escape",
        "dir/file",
        "dir\\file",
        "a\0b",
        "a\nb",
        "report\u{202e}gpj.exe",
        "file\u{2066}name.txt",
    ] {
        assert!(safe_name(name).is_err(), "{name:?}");
    }
    for name in [
        "NUL",
        "con.txt",
        "COM1",
        "LPT².log",
        "file:stream",
        "file.",
        "file ",
        "a?b",
    ] {
        assert!(crate::agent_upload::invalid_windows_name(name), "{name:?}");
    }
    assert_eq!(safe_name(".config").unwrap(), ".config");
    assert_eq!(safe_name("file name.txt").unwrap(), "file name.txt");
}

#[tokio::test]
#[ignore = "manual SFTP latency benchmark"]
async fn listing_latency_benchmark() {
    let (client, proxy_client) = tokio::io::duplex(1024 * 1024);
    let (proxy_server, stream) = tokio::io::duplex(1024 * 1024);
    let (cr, cw) = tokio::io::split(proxy_client);
    let (sr, sw) = tokio::io::split(proxy_server);
    let delay = Duration::from_millis(20);
    tokio::spawn(delayed_link(cr, sw, delay));
    tokio::spawn(delayed_link(sr, cw, delay));
    server::run(stream, DirectoryFixture { batch: 0 }).await;
    let start = Instant::now();
    let session = SftpSession::new(client).await.unwrap();
    let init = start.elapsed();
    let mut sequential = Vec::new();
    let mut overlapped = Vec::new();
    for _ in 0..5 {
        let start = Instant::now();
        let path = session.canonicalize(".").await.unwrap();
        let mut old: Vec<_> = session
            .read_dir(&path)
            .await
            .unwrap()
            .map(|f| f.file_name())
            .collect();
        sequential.push(start.elapsed());
        let start = Instant::now();
        let (path, entries) = list_directory(&session, ".").await.unwrap();
        let mut new: Vec<_> = entries.map(|f| f.file_name()).collect();
        overlapped.push(start.elapsed());
        assert_eq!(path, "/fixture");
        old.sort();
        new.sort();
        assert_eq!(new, old);
        assert_eq!(new.len(), 300);
    }
    sequential.sort();
    overlapped.sort();
    println!(
        "SFTP benchmark (40ms simulated RTT, 300 entries, median of 5): init={:.1}ms sequential={:.1}ms overlapped={:.1}ms",
        init.as_secs_f64() * 1000.0,
        sequential[2].as_secs_f64() * 1000.0,
        overlapped[2].as_secs_f64() * 1000.0
    );
    session.close().await.unwrap();
}
