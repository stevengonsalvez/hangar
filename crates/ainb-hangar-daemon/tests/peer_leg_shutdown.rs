//! The peer leg closes its open sockets 4503 (draining) when the daemon gets
//! SIGTERM, and the close reaches the peer before the process exits.
//!
//! `daemon stop` and `daemon restart` both send SIGTERM. A phone treats any
//! close code it does not know as "do not retry" and stops redialling, so a
//! restart that ended its socket with a bare close or 1001 would strand it.
//! The listener drains its connection tasks, and boot awaits that drain before
//! returning, which is what lets the 4503 win the race with exit.
//!
//! Real binary, isolated home: `HOME` is a temporary directory and the
//! Hangar home is set explicitly inside it (`AINB_HANGAR_HOME`), so nothing
//! touches the operator's fleet. Turning the
//! leg on makes the daemon load its host key, and `load_or_mint` asks the
//! platform keychain (the operator's real one on macOS) only when the 0600 key
//! file is absent. The test seeds that file first, so the keychain is never
//! read or written; the daemon completing a handshake against the seeded key,
//! and recording its public half, is the proof. The child is killed by its own
//! pid if the test fails.

use std::io::Write as _;
use std::net::SocketAddr;
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use ainb_hangar_core::clock::SystemClock;
use ainb_hangar_core::idgen::SystemIdGen;
use ainb_hangar_proto::hosts::{CarrierKind, HostId};
use ainb_hangar_store::Store;
use ainb_hangar_store::repo::daemon_identity::DaemonIdentityRepo;
use futures_util::{SinkExt as _, StreamExt as _};
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

/// Write the host key the daemon will find before it asks any keychain, at
/// the path the daemon itself resolves (`host_key_file_in`), 32 raw
/// bytes, mode 0600. Resolved, not spelled out here, so a change to that path
/// moves the seed with it instead of leaving the daemon to reach a keychain.
fn seed_host_key(hangar_home: &Path, secret: &[u8; 32]) -> std::path::PathBuf {
    let file = ainb_hangar_daemon::host_key_file_in(hangar_home);
    std::fs::create_dir_all(file.parent().expect("parent")).expect("mkdir");
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&file)
        .and_then(|mut f| f.write_all(secret))
        .expect("seed host key");
    file
}

#[tokio::test]
async fn sigterm_closes_open_peer_sockets_4503_before_exit() {
    let home = tempfile::tempdir().expect("home");
    // Named explicitly for the child below, never derived from `HOME`.
    let hangar_home = home.path().join("hangar-home");

    // The host id the device routes to, minted ahead so the test knows it
    // (the daemon reads the same row back), and the host key, seeded.
    let host_id = {
        let store = Store::open_in(&hangar_home).await.expect("store");
        let host_id = DaemonIdentityRepo::mint_or_read(store.pool(), &SystemIdGen, &SystemClock)
            .await
            .expect("mint")
            .identity
            .host_id;
        store.pool().close().await;
        host_id
    };
    let host_key = ainb_hangar_noise::generate_keypair().expect("host key");
    let (host_secret, host_public) = (host_key.private, host_key.public);
    let key_file = seed_host_key(&hangar_home, &host_secret);

    let addr = free_loopback_port();
    let mut daemon = Daemon(
        Command::new(env!("CARGO_BIN_EXE_ainb-hangar-daemon"))
            .env("HOME", home.path())
            .env(ainb_hangar_core::paths::HANGAR_HOME_ENV, &hangar_home)
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

    // Wait for the peer leg to listen (it binds once the host key is loaded).
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

    // Finish the Noise handshake against the seeded key first, so the socket
    // is past its msg1 step and the daemon is known to be serving that key.
    // R1-06a accepts no Rpc hello yet, so the socket then waits in its first
    // Rpc step (2 s); SIGTERM goes out straight after msg2, far inside it.
    let device = ainb_hangar_noise::generate_keypair().expect("device key");
    let mut handshake = ainb_hangar_noise::initiator(
        &device.private,
        &host_public,
        CarrierKind::SshL,
        &HostId::parse_minted(&host_id).expect("host id"),
    )
    .expect("initiator");
    ws.send(Message::Binary(handshake.write_message().expect("msg1")))
        .await
        .expect("send msg1");
    let msg2 = match tokio::time::timeout(Duration::from_secs(10), ws.next()).await {
        Ok(Some(Ok(Message::Binary(bytes)))) => bytes,
        other => panic!("no msg2 from the seeded host key: {other:?}"),
    };
    handshake.read_message(&msg2).expect("msg2");
    handshake.into_session().expect("session");

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
        "a restart must close peer sockets 4503, never 1001, 4401 or a bare close"
    );

    let exited = Instant::now() + Duration::from_secs(20);
    while daemon.0.try_wait().expect("try_wait").is_none() {
        assert!(
            Instant::now() < exited,
            "the daemon did not exit after SIGTERM"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // The daemon kept the seeded key: the file is untouched and the identity
    // row records its public half, so no keychain key was minted or read.
    assert_eq!(
        std::fs::read(&key_file).expect("key file"),
        host_secret.as_slice()
    );
    let store = Store::open_in(&hangar_home).await.expect("reopen store");
    assert_eq!(
        DaemonIdentityRepo::host_static_pubkey(store.pool()).await.expect("pubkey"),
        Some(host_public.to_vec())
    );
}
