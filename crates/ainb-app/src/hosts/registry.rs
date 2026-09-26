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
    /// The carrier of the live connection, if there is one.
    #[must_use]
    pub const fn carrier(&self) -> Option<CarrierKind> {
        match self.reachability {
            Reachability::Reachable { carrier } => Some(carrier),
            _ => None,
        }
    }
}

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

    /// Add a host, or return the one already there. A new host is
    /// unreachable since it was added, until a read reaches it.
    pub fn upsert(&mut self, host_id: HostId, kind: HostKind, now_ms: i64) -> &mut HostApp {
        let at = if let Some(at) = self.hosts.iter().position(|h| h.host_id == host_id) {
            at
        } else {
            self.hosts.push(HostApp {
                host_id,
                kind,
                reachability: Reachability::Unreachable { since_ms: now_ms },
                last_contact_ms: None,
                added_at_ms: now_ms,
                last_active_ms: now_ms,
            });
            self.hosts.len() - 1
        };
        &mut self.hosts[at]
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
        r.upsert(id(1), HostKind::Remote, 100);
        let host = r.get(&id(1)).expect("added");
        assert_eq!(
            host.reachability,
            Reachability::Unreachable { since_ms: 100 }
        );
        assert_eq!(host.carrier(), None);
        let again = r.upsert(id(1), HostKind::Remote, 999);
        assert_eq!(again.added_at_ms, 100, "upsert keeps the existing host");
        assert_eq!(r.len(), 1);
    }

    #[test]
    fn lost_is_since_the_last_contact_and_keeps_its_first_since() {
        let mut r = HostRegistry::new();
        r.upsert(id(1), HostKind::Remote, 100);
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
        r.upsert(id(1), HostKind::Remote, 100);
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
        r.upsert(id(3), HostKind::Remote, 10);
        r.upsert(id(2), HostKind::Remote, 10);
        r.upsert(HostId::local(), HostKind::Local, 10);
        r.upsert(id(1), HostKind::Remote, 10);
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
        r.upsert(HostId::local(), HostKind::Local, 0);
        r.upsert(id(1), HostKind::Remote, 0);
        assert!(r.remove(&HostId::local()).is_none());
        assert_eq!(r.remove(&id(1)).map(|h| h.host_id), Some(id(1)));
        assert_eq!(r.len(), 1);
        assert!(!r.is_empty());
    }
}
