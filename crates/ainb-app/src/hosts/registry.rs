// ABOUTME: The per-surface list of hosts and how reachable each one is right now.
// Reachability is a field of the host, never folded into a row's state: a row
// from an unreachable host keeps its last known state and the host shows the
// badge beside it.
//
//            reached(carrier)             lost()
//   ┌──────────────┐ ◀──────── ┌───────────────┐ ────────▶ ┌─────────────────┐
//   │ Reachable    │           │ (any state)   │           │ Unreachable     │
//   │ { carrier }  │ ────────▶ │               │           │ { since: last   │
//   └──────────────┘  behind() └───────────────┘           │   contact }     │
//          ▼                                               └─────────────────┘
//   ┌──────────────────────────┐
//   │ Stale { last_seq, since }│   since = when it first fell behind
//   └──────────────────────────┘

use ainb_hangar_proto::hosts::{CarrierKind, HostId, Reachability};

/// Whether a host is this machine's own daemon or a paired one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HostKind {
    /// The daemon on this machine, over the unix socket.
    Local,
    /// A paired daemon, over the peer leg. Never read through the local
    /// socket, and never given local effects.
    Remote,
}

/// One host as a surface sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostApp {
    /// The id the host's daemon named at hello.
    pub host_id: HostId,
    /// Local or paired.
    pub kind: HostKind,
    /// Reachable right now, or since when not.
    pub reachability: Reachability,
    /// Unix milliseconds of the last successful contact; `None` if never.
    pub last_contact_ms: Option<i64>,
    /// Unix milliseconds the host joined this registry.
    pub added_at_ms: i64,
    /// Unix milliseconds the user last acted on this host. Orders the
    /// switcher and, in the client, who resyncs first.
    pub last_active_ms: i64,
}

impl HostApp {
    const fn new(host_id: HostId, kind: HostKind, now_ms: i64) -> Self {
        Self {
            host_id,
            kind,
            reachability: Reachability::Unreachable { since_ms: now_ms },
            last_contact_ms: None,
            added_at_ms: now_ms,
            last_active_ms: now_ms,
        }
    }

    /// The carrier of the live connection, if there is one.
    #[must_use]
    pub const fn carrier(&self) -> Option<CarrierKind> {
        match self.reachability {
            Reachability::Reachable { carrier } => Some(carrier),
            _ => None,
        }
    }
}

/// The most paired hosts one surface keeps. Every paired host costs a
/// persistent session, a resync and a census listing, so the list is bounded.
pub const MAX_PAIRED_HOSTS: usize = 16;

/// Why the registry refused a host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegistryError {
    /// [`MAX_PAIRED_HOSTS`] paired hosts are already known.
    Full {
        /// The cap.
        cap: usize,
    },
    /// The id is `local` or this machine's own host: a host is never paired
    /// with itself.
    LocalIsPaired(HostId),
}

impl std::fmt::Display for RegistryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Full { cap } => write!(f, "at most {cap} paired hosts"),
            Self::LocalIsPaired(id) => write!(f, "{id} is this machine's own host"),
        }
    }
}

impl std::error::Error for RegistryError {}

/// Every host a surface knows, local first.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HostRegistry {
    hosts: Vec<HostApp>,
}

impl HostRegistry {
    /// An empty registry.
    #[must_use]
    pub const fn new() -> Self {
        Self { hosts: Vec::new() }
    }

    /// Name this machine's own host, keyed by the id its daemon named at
    /// hello (the minted ULID), or `local` while it has named none.
    ///
    /// There is one local host. Called again with a new id (the daemon minted
    /// after the surface started), it re-keys that host and keeps its state.
    /// The key matters: the daemon stamps every row with its minted id, and
    /// [`super::census::listing_from_read`] refuses rows that name another
    /// host, so a local host still keyed `local` would refuse its own rows.
    ///
    /// A paired host already known under this id is this machine seen through
    /// the peer leg (paired before the local daemon named itself). It is
    /// dropped, and the local host takes the id: a host is never paired with
    /// itself, and refusing here would leave the local host keyed `local`.
    pub fn set_local(&mut self, host_id: HostId, now_ms: i64) -> &mut HostApp {
        if let Some(at) = self
            .hosts
            .iter()
            .position(|h| h.kind == HostKind::Remote && h.host_id == host_id)
        {
            self.hosts.remove(at);
        }
        let at = if let Some(at) = self.hosts.iter().position(|h| h.kind == HostKind::Local) {
            self.hosts[at].host_id = host_id;
            at
        } else {
            self.hosts.push(HostApp::new(host_id, HostKind::Local, now_ms));
            self.hosts.len() - 1
        };
        &mut self.hosts[at]
    }

    /// Add a paired host, or return the one already there. A new host is
    /// unreachable since it was added, until a read reaches it.
    ///
    /// Refused: `local` (a peer is always a minted host), this machine's own
    /// id, and a new host past [`MAX_PAIRED_HOSTS`].
    pub fn upsert_remote(
        &mut self,
        host_id: HostId,
        now_ms: i64,
    ) -> Result<&mut HostApp, RegistryError> {
        if host_id.is_local() {
            return Err(RegistryError::LocalIsPaired(host_id));
        }
        if let Some(at) = self.hosts.iter().position(|h| h.host_id == host_id) {
            if self.hosts[at].kind == HostKind::Local {
                return Err(RegistryError::LocalIsPaired(host_id));
            }
            return Ok(&mut self.hosts[at]);
        }
        let paired = self.hosts.iter().filter(|h| h.kind == HostKind::Remote).count();
        if paired >= MAX_PAIRED_HOSTS {
            return Err(RegistryError::Full {
                cap: MAX_PAIRED_HOSTS,
            });
        }
        self.hosts.push(HostApp::new(host_id, HostKind::Remote, now_ms));
        let at = self.hosts.len() - 1;
        Ok(&mut self.hosts[at])
    }

    /// This machine's own host, once named.
    #[must_use]
    pub fn local(&self) -> Option<&HostApp> {
        self.hosts.iter().find(|h| h.kind == HostKind::Local)
    }

    /// The host `host_id`, if known.
    #[must_use]
    pub fn get(&self, host_id: &HostId) -> Option<&HostApp> {
        self.hosts.iter().find(|h| &h.host_id == host_id)
    }

    fn get_mut(&mut self, host_id: &HostId) -> Option<&mut HostApp> {
        self.hosts.iter_mut().find(|h| &h.host_id == host_id)
    }

    /// A read or a live session reached `host_id` over `carrier`. Returns
    /// false for a host this registry does not know.
    pub fn reached(&mut self, host_id: &HostId, carrier: CarrierKind, now_ms: i64) -> bool {
        let Some(host) = self.get_mut(host_id) else {
            return false;
        };
        host.reachability = Reachability::Reachable { carrier };
        host.last_contact_ms = Some(now_ms);
        true
    }

    /// A read or a live session to `host_id` failed. The host is unreachable
    /// since its last successful contact (or since it was added, if never),
    /// and an already unreachable host keeps its first `since`.
    pub fn lost(&mut self, host_id: &HostId) -> bool {
        let Some(host) = self.get_mut(host_id) else {
            return false;
        };
        if !matches!(host.reachability, Reachability::Unreachable { .. }) {
            host.reachability = Reachability::Unreachable {
                since_ms: host.last_contact_ms.unwrap_or(host.added_at_ms),
            };
        }
        true
    }

    /// The live stream from `host_id` is connected but behind at `last_seq`.
    /// `since` is when it first fell behind; a later call only moves
    /// `last_seq`.
    pub fn behind(&mut self, host_id: &HostId, last_seq: i64, now_ms: i64) -> bool {
        let Some(host) = self.get_mut(host_id) else {
            return false;
        };
        let since_ms = match host.reachability {
            Reachability::Stale { since_ms, .. } => since_ms,
            _ => now_ms,
        };
        host.reachability = Reachability::Stale { last_seq, since_ms };
        true
    }

    /// The user acted on `host_id`.
    pub fn touch(&mut self, host_id: &HostId, now_ms: i64) -> bool {
        let Some(host) = self.get_mut(host_id) else {
            return false;
        };
        host.last_active_ms = host.last_active_ms.max(now_ms);
        true
    }

    /// Forget a paired host. The local host cannot be removed.
    pub fn remove(&mut self, host_id: &HostId) -> Option<HostApp> {
        let at = self
            .hosts
            .iter()
            .position(|h| &h.host_id == host_id && h.kind == HostKind::Remote)?;
        Some(self.hosts.remove(at))
    }

    /// Every host: local first, then paired hosts most recently active
    /// first, ties by id so the order is total.
    #[must_use]
    pub fn ordered(&self) -> Vec<&HostApp> {
        let mut hosts: Vec<&HostApp> = self.hosts.iter().collect();
        hosts.sort_by(|a, b| {
            (a.kind == HostKind::Remote)
                .cmp(&(b.kind == HostKind::Remote))
                .then(b.last_active_ms.cmp(&a.last_active_ms))
                .then(a.host_id.cmp(&b.host_id))
        });
        hosts
    }

    /// How many hosts.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.hosts.len()
    }

    /// Whether no host is known.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.hosts.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(n: u8) -> HostId {
        HostId::parse(&format!("01K5A0000000000000000ABC{n:02}")).expect("a ULID")
    }

    #[test]
    fn a_new_host_is_unreachable_since_it_was_added() {
        let mut r = HostRegistry::new();
        r.upsert_remote(id(1), 100).unwrap();
        let host = r.get(&id(1)).expect("added");
        assert_eq!(
            host.reachability,
            Reachability::Unreachable { since_ms: 100 }
        );
        assert_eq!(host.carrier(), None);
        let again = r.upsert_remote(id(1), 999).unwrap();
        assert_eq!(again.added_at_ms, 100, "upsert keeps the existing host");
        assert_eq!(r.len(), 1);
    }

    #[test]
    fn lost_is_since_the_last_contact_and_keeps_its_first_since() {
        let mut r = HostRegistry::new();
        r.upsert_remote(id(1), 100).unwrap();
        assert!(r.reached(&id(1), CarrierKind::Tailnet, 200));
        assert_eq!(r.get(&id(1)).unwrap().carrier(), Some(CarrierKind::Tailnet));
        assert!(r.lost(&id(1)));
        assert_eq!(
            r.get(&id(1)).unwrap().reachability,
            Reachability::Unreachable { since_ms: 200 }
        );
        assert!(r.lost(&id(1)));
        assert_eq!(
            r.get(&id(1)).unwrap().reachability,
            Reachability::Unreachable { since_ms: 200 }
        );
        assert!(!r.lost(&id(9)), "an unknown host is not invented");
        assert!(r.get(&id(9)).is_none());
    }

    #[test]
    fn behind_keeps_when_it_first_fell_behind() {
        let mut r = HostRegistry::new();
        r.upsert_remote(id(1), 100).unwrap();
        r.reached(&id(1), CarrierKind::SshL, 150);
        r.behind(&id(1), 7, 300);
        r.behind(&id(1), 9, 400);
        assert_eq!(
            r.get(&id(1)).unwrap().reachability,
            Reachability::Stale {
                last_seq: 9,
                since_ms: 300
            }
        );
        r.reached(&id(1), CarrierKind::SshL, 500);
        r.behind(&id(1), 10, 600);
        assert_eq!(
            r.get(&id(1)).unwrap().reachability,
            Reachability::Stale {
                last_seq: 10,
                since_ms: 600
            }
        );
    }

    #[test]
    fn local_comes_first_then_the_most_recently_active() {
        let mut r = HostRegistry::new();
        r.upsert_remote(id(3), 10).unwrap();
        r.upsert_remote(id(2), 10).unwrap();
        r.set_local(HostId::local(), 10);
        r.upsert_remote(id(1), 10).unwrap();
        r.touch(&id(3), 50);
        let order: Vec<&HostId> = r.ordered().iter().map(|h| &h.host_id).collect();
        assert_eq!(order, vec![&HostId::local(), &id(3), &id(1), &id(2)]);
        r.touch(&id(3), 20);
        assert_eq!(
            r.get(&id(3)).unwrap().last_active_ms,
            50,
            "never moves back"
        );
    }

    #[test]
    fn only_a_paired_host_can_be_removed() {
        let mut r = HostRegistry::new();
        r.set_local(HostId::local(), 0);
        r.upsert_remote(id(1), 0).unwrap();
        assert!(r.remove(&HostId::local()).is_none());
        assert_eq!(r.remove(&id(1)).map(|h| h.host_id), Some(id(1)));
        assert_eq!(r.len(), 1);
        assert!(!r.is_empty());
    }

    /// The local host is keyed by the id its daemon named, and re-keyed in
    /// place when the daemon mints after the surface started.
    #[test]
    fn the_local_host_is_re_keyed_to_its_minted_id() {
        let mut r = HostRegistry::new();
        r.set_local(HostId::local(), 10);
        r.reached(&HostId::local(), CarrierKind::SshL, 20);
        let minted = id(7);
        r.set_local(minted.clone(), 30);
        assert_eq!(r.len(), 1, "one local host, re-keyed");
        let local = r.local().expect("local");
        assert_eq!(local.host_id, minted);
        assert_eq!(local.added_at_ms, 10, "its state is kept");
        assert_eq!(local.last_contact_ms, Some(20));
        assert!(r.get(&HostId::local()).is_none());
    }

    #[test]
    fn a_host_is_never_paired_with_itself() {
        let mut r = HostRegistry::new();
        r.set_local(id(1), 0);
        assert_eq!(
            r.upsert_remote(id(1), 0).unwrap_err(),
            RegistryError::LocalIsPaired(id(1))
        );
        assert_eq!(
            r.upsert_remote(HostId::local(), 0).unwrap_err(),
            RegistryError::LocalIsPaired(HostId::local())
        );
    }

    /// The order the review found: this machine was paired as a remote under
    /// its minted id before the local daemon named itself. Naming the local
    /// host then drops that remote and re-keys the local host, which keeps
    /// its own state, so its rows are no longer refused.
    #[test]
    fn naming_the_local_host_drops_a_remote_paired_under_its_id() {
        let mut r = HostRegistry::new();
        r.set_local(HostId::local(), 10);
        r.reached(&HostId::local(), CarrierKind::SshL, 20);
        r.upsert_remote(id(5), 30).unwrap();
        r.upsert_remote(id(6), 30).unwrap();

        let local = r.set_local(id(5), 40);
        assert_eq!(local.kind, HostKind::Local);
        assert_eq!(local.host_id, id(5));
        assert_eq!(local.added_at_ms, 10, "the local host keeps its own state");
        assert_eq!(local.last_contact_ms, Some(20));

        assert_eq!(r.len(), 2, "the remote under that id is gone");
        assert_eq!(r.get(&id(5)).map(|h| h.kind), Some(HostKind::Local));
        assert_eq!(r.get(&id(6)).map(|h| h.kind), Some(HostKind::Remote));
        assert!(r.get(&HostId::local()).is_none());
        assert_eq!(
            r.upsert_remote(id(5), 50).unwrap_err(),
            RegistryError::LocalIsPaired(id(5)),
            "and it cannot be paired again"
        );
    }

    #[test]
    fn paired_hosts_are_capped_and_the_local_host_does_not_count() {
        let mut r = HostRegistry::new();
        r.set_local(id(0), 0);
        for n in 1..=u8::try_from(MAX_PAIRED_HOSTS).unwrap() {
            r.upsert_remote(id(n), 0).unwrap();
        }
        assert_eq!(r.len(), MAX_PAIRED_HOSTS + 1);
        assert_eq!(
            r.upsert_remote(id(99), 0).unwrap_err(),
            RegistryError::Full {
                cap: MAX_PAIRED_HOSTS
            }
        );
        assert!(r.upsert_remote(id(1), 5).is_ok(), "a known host is not new");
        r.remove(&id(1));
        assert!(r.upsert_remote(id(99), 0).is_ok(), "room after a remove");
    }
}
