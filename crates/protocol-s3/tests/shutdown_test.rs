//! `S3Server::shutdown_on`: what a server does between its shutdown signal
//! and returning, over real sockets.

use capsule_registry::modular_pipeline::StoreSecurity;
use capsule_registry::CapsuleRegistry;
use nvram_sim::NvramLog;
use protocol_s3::{server::S3Server, S3View};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio::time::timeout;

/// Far longer than any of this should take, so a slow CI machine doesn't
/// fail a test, while a server that never stops still does.
const PATIENCE: Duration = Duration::from_secs(20);

struct Running {
    dir: PathBuf,
    addr: SocketAddr,
    stop: Option<oneshot::Sender<()>>,
    server: JoinHandle<anyhow::Result<()>>,
}

impl Running {
    async fn start(name: &str, grace: Duration) -> Self {
        let dir = std::env::temp_dir().join(format!("s3_shutdown_{name}_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let view = open_view(&dir);
        view.create_bucket("demo").unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (stop, stopped) = oneshot::channel::<()>();
        let server = S3Server::new(view, vec![listener]).shutdown_on(
            async move {
                let _ = stopped.await;
            },
            grace,
        );
        Self {
            dir,
            addr,
            stop: Some(stop),
            server: tokio::spawn(server.run()),
        }
    }

    fn signal(&mut self) {
        self.stop.take().unwrap().send(()).unwrap();
    }

    /// Waits for the server to return, and says how long it took.
    async fn stopped(self) -> (PathBuf, Duration) {
        let started = Instant::now();
        timeout(PATIENCE, self.server)
            .await
            .expect("the server didn't stop")
            .expect("the server task panicked")
            .expect("the server failed");
        (self.dir, started.elapsed())
    }
}

fn open_view(dir: &Path) -> S3View {
    let registry = CapsuleRegistry::open(dir.join("space.db")).unwrap();
    let log = NvramLog::open(dir.join("space.nvram")).unwrap();
    S3View::new(registry, log, &StoreSecurity::none()).unwrap()
}

/// Reads one response's head off a connection that stays open.
async fn read_head(stream: &mut TcpStream) -> String {
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        let n = timeout(PATIENCE, stream.read(&mut byte))
            .await
            .expect("no response")
            .unwrap();
        assert_eq!(n, 1, "connection closed mid-response: {head:?}");
        head.push(byte[0]);
    }
    String::from_utf8(head).unwrap()
}

#[tokio::test]
async fn an_idle_connection_is_closed_and_doesnt_hold_up_the_stop() {
    let mut running = Running::start("idle", PATIENCE).await;
    let mut idle = TcpStream::connect(running.addr).await.unwrap();
    idle.write_all(b"GET /-/health HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n")
        .await
        .unwrap();
    let head = read_head(&mut idle).await;
    assert!(head.starts_with("HTTP/1.1 200"), "{head}");
    // Drain the small body, leaving a keep-alive connection with nothing in
    // flight.
    tokio::time::sleep(Duration::from_millis(100)).await;
    let mut body = [0u8; 256];
    let _ = timeout(Duration::from_millis(500), idle.read(&mut body)).await;

    running.signal();
    let addr = running.addr;
    let (_, took) = running.stopped().await;
    assert!(
        took < PATIENCE / 2,
        "waited on an idle connection: {took:?}"
    );

    let mut rest = Vec::new();
    let closed = timeout(PATIENCE, idle.read_to_end(&mut rest)).await;
    assert!(
        matches!(closed, Ok(Ok(_)) | Ok(Err(_))),
        "the idle connection was left open"
    );
    // Nothing listens any more.
    assert!(
        !matches!(timeout(PATIENCE, TcpStream::connect(addr)).await, Ok(Ok(_))),
        "still accepting connections after stopping"
    );
}

#[tokio::test]
async fn a_request_in_flight_at_the_signal_finishes() {
    let mut running = Running::start("in_flight", PATIENCE).await;
    let mut put = TcpStream::connect(running.addr).await.unwrap();
    put.write_all(b"PUT /demo/k HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: 10\r\n\r\nhello")
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;

    running.signal();
    tokio::time::sleep(Duration::from_millis(200)).await;
    put.write_all(b"world").await.unwrap();
    let head = read_head(&mut put).await;
    assert!(head.starts_with("HTTP/1.1 200"), "{head}");

    let (dir, _) = running.stopped().await;
    // The server let go of the store, and the PUT is in it.
    let view = open_view(&dir);
    assert_eq!(view.head_object("demo", "k").unwrap().size(), 10);
}

#[tokio::test]
async fn a_request_still_running_after_the_grace_period_is_cut_off() {
    let grace = Duration::from_millis(300);
    let mut running = Running::start("cut_off", grace).await;
    let mut put = TcpStream::connect(running.addr).await.unwrap();
    // A body that never arrives in full.
    put.write_all(b"PUT /demo/k HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: 10\r\n\r\nhello")
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;

    running.signal();
    // Cut off with a 503, which S3 clients retry.
    let head = read_head(&mut put).await;
    assert!(head.starts_with("HTTP/1.1 503"), "{head}");
    let (dir, took) = running.stopped().await;
    assert!(
        took < PATIENCE / 2,
        "didn't stop after the grace period: {took:?}"
    );

    // The server let go of the store, and nothing of the PUT is in it.
    let view = open_view(&dir);
    assert!(
        view.head_object("demo", "k").is_err(),
        "half a PUT was stored"
    );
}
