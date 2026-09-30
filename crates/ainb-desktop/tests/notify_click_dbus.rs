//! A notification's click, end to end on Linux: the window's own
//! `OsDelivery` sends through notify-rust to a notification daemon on a
//! private session bus, the daemon reports a click, and the click reaches the
//! notice's session. No desktop draws a banner here: the fake daemon stands in
//! for the one that would, speaking the same `org.freedesktop.Notifications`
//! interface. Its own test binary, because it points this process's session
//! bus at the private one.
#![cfg(target_os = "linux")]

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use ainb_desktop::notify::{Notice, NoticeKind};
use ainb_desktop::notify_delivery::{Deliver, MAX_WAITERS, OsDelivery};
use zbus::object_server::SignalEmitter;
use zbus::zvariant::OwnedValue;

const PATH: &str = "/org/freedesktop/Notifications";

/// One notification as the daemon received it.
#[derive(Debug, Clone)]
struct Shown {
    id: u32,
    summary: String,
    body: String,
    actions: Vec<String>,
}

/// The fake notification daemon: records each notification, and emits the
/// click and close signals a real one emits when the test asks.
#[derive(Default, Clone)]
struct Daemon {
    shown: Arc<Mutex<Vec<Shown>>>,
}

#[zbus::interface(name = "org.freedesktop.Notifications")]
impl Daemon {
    #[allow(clippy::too_many_arguments)]
    fn notify(
        &self,
        _app_name: String,
        _replaces_id: u32,
        _app_icon: String,
        summary: String,
        body: String,
        actions: Vec<String>,
        _hints: HashMap<String, OwnedValue>,
        _expire_timeout: i32,
    ) -> u32 {
        let mut shown = self.shown.lock().unwrap();
        let id = u32::try_from(shown.len()).unwrap() + 1;
        shown.push(Shown {
            id,
            summary,
            body,
            actions,
        });
        id
    }

    fn close_notification(&self, _id: u32) {}

    fn get_capabilities(&self) -> Vec<String> {
        vec!["actions".to_string(), "body".to_string()]
    }

    fn get_server_information(&self) -> (String, String, String, String) {
        (
            "fake".to_string(),
            "ainb".to_string(),
            "1".to_string(),
            "1.2".to_string(),
        )
    }

    #[zbus(signal)]
    async fn action_invoked(
        emitter: &SignalEmitter<'_>,
        id: u32,
        action_key: &str,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn notification_closed(
        emitter: &SignalEmitter<'_>,
        id: u32,
        reason: u32,
    ) -> zbus::Result<()>;
}

/// A private session bus, stopped when dropped.
struct Bus {
    daemon: Child,
    address: String,
    _dir: tempfile::TempDir,
}

impl Bus {
    fn start() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let socket: PathBuf = dir.path().join("bus");
        let address = format!("unix:path={}", socket.display());
        let daemon = Command::new("dbus-daemon")
            .args(["--session", "--nofork", "--address", &address])
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("dbus-daemon runs this test's private session bus");
        until(|| socket.exists(), "the private bus socket");
        Self {
            daemon,
            address,
            _dir: dir,
        }
    }
}

impl Drop for Bus {
    fn drop(&mut self) {
        let _ = self.daemon.kill();
        let _ = self.daemon.wait();
    }
}

fn until(ready: impl Fn() -> bool, what: &str) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !ready() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn notice(session: &str) -> Notice {
    Notice {
        kind: NoticeKind::Needs,
        session_key: Some(session.to_string()),
        title: "api".to_string(),
        body: "Needs you: it asked a question".to_string(),
    }
}

#[test]
fn a_click_on_the_os_notification_reaches_its_session_and_every_waiter_ends() {
    let bus = Bus::start();
    // The only test in this binary, and before anything here dials a bus.
    std::env::set_var("DBUS_SESSION_BUS_ADDRESS", &bus.address);
    let daemon = Daemon::default();
    let connection = zbus::blocking::connection::Builder::address(bus.address.as_str())
        .unwrap()
        .name("org.freedesktop.Notifications")
        .unwrap()
        .serve_at(PATH, daemon.clone())
        .unwrap()
        .build()
        .unwrap();
    let interface = connection.object_server().interface::<_, Daemon>(PATH).unwrap();
    let emitter = interface.signal_emitter();
    let shown = |count: usize| daemon.shown.lock().unwrap().len() >= count;

    // Clicked: the session reaches the click, once, and the waiter ends.
    let os = OsDelivery::default();
    let (clicks, clicked) = mpsc::channel();
    let tx = clicks.clone();
    os.deliver(
        notice("claude:a"),
        Box::new(move || tx.send("claude:a").unwrap()),
    );
    until(|| shown(1), "the daemon to receive the notification");
    let first = daemon.shown.lock().unwrap()[0].clone();
    assert_eq!(first.summary, "api");
    assert_eq!(first.body, "Needs you: it asked a question");
    assert!(
        first.actions.contains(&"default".to_string()),
        "a click on the banner is the default action: {:?}",
        first.actions
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    let session = loop {
        // Again until heard: the waiter subscribes a moment after the send.
        zbus::block_on(Daemon::action_invoked(emitter, first.id, "default")).unwrap();
        if let Ok(session) = clicked.recv_timeout(Duration::from_millis(100)) {
            break session;
        }
        assert!(
            Instant::now() < deadline,
            "the click never reached the window"
        );
    };
    assert_eq!(session, "claude:a");
    until(|| os.waiting() == 0, "the clicked notice's waiter to end");

    // Dismissed: no click, and the waiter ends.
    let tx = clicks.clone();
    os.deliver(
        notice("claude:b"),
        Box::new(move || tx.send("claude:b").unwrap()),
    );
    until(|| shown(2), "the second notification");
    let second = daemon.shown.lock().unwrap()[1].id;
    until(|| os.waiting() == 1, "the second notice to wait");
    let deadline = Instant::now() + Duration::from_secs(10);
    while os.waiting() > 0 {
        zbus::block_on(Daemon::notification_closed(emitter, second, 2)).unwrap();
        std::thread::sleep(Duration::from_millis(50));
        assert!(Instant::now() < deadline, "a dismissed notice kept waiting");
    }
    assert!(clicked.try_recv().is_err(), "a dismissal is not a click");

    // Bounded: past MAX_WAITERS a notice is still shown, and nothing more waits.
    let burst = MAX_WAITERS + 2;
    for n in 0..burst {
        os.deliver(notice(&format!("claude:{n}")), Box::new(|| {}));
    }
    until(
        || shown(2 + burst),
        "every notice past the cap to be shown too",
    );
    until(|| os.waiting() == MAX_WAITERS, "the cap's waiters");
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(os.waiting(), MAX_WAITERS, "never more than the cap");
    let ids: Vec<u32> = daemon.shown.lock().unwrap()[2..].iter().map(|shown| shown.id).collect();
    let deadline = Instant::now() + Duration::from_secs(10);
    while os.waiting() > 0 {
        for id in &ids {
            zbus::block_on(Daemon::notification_closed(emitter, *id, 1)).unwrap();
        }
        std::thread::sleep(Duration::from_millis(50));
        assert!(Instant::now() < deadline, "expired notices kept waiting");
    }
}
