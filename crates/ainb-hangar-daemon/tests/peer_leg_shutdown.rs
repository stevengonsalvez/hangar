//! The peer leg closes its open sockets 4503 (draining) when the daemon gets
//! SIGTERM, and the close reaches the peer before the process exits.
//!
//! `daemon stop` and `daemon restart` both send SIGTERM. A phone treats any
//! close code it does not know as "do not retry" and stops redialling, so a
//! restart that ended its socket with a bare close or 1001 would strand it.
//! The listener drains its connection tasks, and boot awaits that drain before
//! returning, which is what lets the 4503 win the race with exit.
//!
//! Real binary, isolated home: `HOME` is a temporary directory and the home
//! overrides are removed, so nothing touches the operator's fleet. The child
//! is killed by its own pid if the test fails.

use std::net::SocketAddr;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use futures_util::StreamExt as _;
use tokio_tungstenite::tungstenite::Message;

/// The daemon child, killed by its own pid when the test ends.
struct Daemon(Child);

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn free_loopback_port() -> SocketAddr {
    let probe = std::net::TcpListener::bind("127.0.0.1:0").expect("probe bind");
    probe.local_addr().expect("probe addr")
}

#[tokio::test]
async fn sigterm_closes_open_peer_sockets_4503_before_exit() {
    let home = tempfile::tempdir().expect("home");
    let addr = free_loopback_port();
    let mut daemon = Daemon(
        Command::new(env!("CARGO_BIN_EXE_ainb-hangar-daemon"))
            .env("HOME", home.path())
            .env_remove("AINB_HANGAR_HOME")
            .env_remove("AINB_HOME")
            .env("HANGAR_TEST_PARENT_PID", std::process::id().to_string())
            .env("HANGAR_DAEMON_DISABLE_CLAIM", "1")
            .env("AINB_HANGAR_PEER_LISTEN", addr.to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn ainb-hangar-daemon"),
    );

    // Wait for the peer leg to listen (it binds once the host key is minted).
    let deadline = Instant::now() + Duration::from_secs(60);
    let tcp = loop {
        if let Ok(tcp) = tokio::net::TcpStream::connect(addr).await {
            break tcp;
        }
        assert!(
            daemon.0.try_wait().expect("try_wait").is_none(),
            "the daemon exited before its peer leg listened"
        );
        assert!(
            Instant::now() < deadline,
            "the peer leg never listened on {addr}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    let (mut ws, _) = tokio_tungstenite::client_async(format!("ws://{addr}/peer"), tcp)
        .await
        .expect("upgrade");

    // An open, pre-auth socket (well inside its 2 s step), then SIGTERM.
    let pid = nix::unistd::Pid::from_raw(i32::try_from(daemon.0.id()).expect("pid"));
    nix::sys::signal::kill(pid, nix::sys::signal::Signal::SIGTERM).expect("SIGTERM");

    let code = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match ws.next().await {
                Some(Ok(Message::Close(frame))) => return frame.map(|f| u16::from(f.code)),
                Some(Ok(_)) => {}
                Some(Err(_)) | None => return None,
            }
        }
    })
    .await
    .expect("the socket closes after SIGTERM");
    assert_eq!(
        code,
        Some(ainb_hangar_proto::peer_close::DRAINING),
        "a restart must close peer sockets 4503, never 1001 or a bare close"
    );

    let exited = Instant::now() + Duration::from_secs(20);
    while daemon.0.try_wait().expect("try_wait").is_none() {
        assert!(
            Instant::now() < exited,
            "the daemon did not exit after SIGTERM"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
