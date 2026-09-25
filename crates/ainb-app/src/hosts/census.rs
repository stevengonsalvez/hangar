// ABOUTME: The coverage census: many hosts' listings folded into one capped list,
// with one coverage state per host so a surface always says what it left out.
//
//   host A: Fresh  [a1 a2 a3 a4]  ┐
//   host B: Stale  [b1]           ├─ fold(cap = 4) ─▶ rows  A:a1 a2 a3 · B:b1(stale)
//   host C: Unreachable{since}    ┘                   A: omitted_by_cap{1}
//                                                     B: stale{head_revision, since}
//                                                     C: unreachable{since}
//
// Rows are picked round robin, one per host per pass, so a busy host cannot
// crowd a quiet one out of the cap. The picked rows come back grouped by host,
// in listing order. A stale host's rows are never merged without the badge:
// every one carries `stale: true`, and the host's coverage is `stale` even
// when the cap also cut it (staleness is the state a user must not miss).

use std::collections::BTreeSet;

use ainb_hangar_client::DaemonClient;
use ainb_hangar_proto::agent_status::RosterStatusRow;
use ainb_hangar_proto::hosts::{CarrierKind, HostCoverage, HostId};

use super::registry::HostRegistry;

/// What one host contributed to a listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostListing<R> {
    /// A read that just succeeded.
    Fresh {
        /// The rows, in the host's own order.
        rows: Vec<R>,
        /// The revision the read was taken at.
        head_revision: i64,
    },
    /// Rows held from an earlier read while the live stream is behind.
    Stale {
        /// The rows held.
        rows: Vec<R>,
        /// The revision they were read at.
        head_revision: i64,
        /// Unix milliseconds since the stream fell behind.
        since_ms: i64,
    },
    /// No rows: the host did not answer.
    Unreachable {
        /// Unix milliseconds of the last successful contact.
        since_ms: i64,
    },
}

/// One row of the folded list, tagged with where it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CensusRow<R> {
    /// The host the row belongs to.
    pub host_id: HostId,
    /// Whether the row is from a stale listing (show the badge).
    pub stale: bool,
    /// The row.
    pub row: R,
}

/// The folded list and each host's coverage in it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Census<R> {
    /// The rows kept, grouped by host in listing order.
    pub rows: Vec<CensusRow<R>>,
    /// One coverage state per host, in listing order.
    pub coverage: Vec<(HostId, HostCoverage)>,
}

impl<R> Census<R> {
    /// The coverage of `host_id`, if it was in the listing.
    #[must_use]
    pub fn coverage_of(&self, host_id: &HostId) -> Option<HostCoverage> {
        self.coverage
            .iter()
            .find(|(id, _)| id == host_id)
            .map(|(_, coverage)| *coverage)
    }
}

/// Fold per-host listings into at most `cap` rows. A host listed twice keeps
/// its first listing only.
#[must_use]
pub fn fold<R>(listings: Vec<(HostId, HostListing<R>)>, cap: usize) -> Census<R> {
    let mut seen = BTreeSet::new();
    let listings: Vec<(HostId, HostListing<R>)> =
        listings.into_iter().filter(|(id, _)| seen.insert(id.clone())).collect();

    // Round robin: how many rows each host keeps.
    let available: Vec<usize> = listings
        .iter()
        .map(|(_, listing)| match listing {
            HostListing::Fresh { rows, .. } | HostListing::Stale { rows, .. } => rows.len(),
            HostListing::Unreachable { .. } => 0,
        })
        .collect();
    let mut keep = vec![0usize; listings.len()];
    let mut left = cap;
    while left > 0 {
        let mut progressed = false;
        for (at, kept) in keep.iter_mut().enumerate() {
            if left == 0 {
                break;
            }
            if *kept < available[at] {
                *kept += 1;
                left -= 1;
                progressed = true;
            }
        }
        if !progressed {
            break;
        }
    }

    let mut rows = Vec::new();
    let mut coverage = Vec::with_capacity(listings.len());
    for ((host_id, listing), kept) in listings.into_iter().zip(keep) {
        let (host_rows, stale, state) = match listing {
            HostListing::Unreachable { since_ms } => {
                coverage.push((host_id, HostCoverage::Unreachable { since_ms }));
                continue;
            }
            HostListing::Fresh { rows, .. } => {
                let omitted = rows.len() - kept;
                let state = if omitted == 0 {
                    HostCoverage::Covered
                } else {
                    HostCoverage::OmittedByCap {
                        omitted: u32::try_from(omitted).unwrap_or(u32::MAX),
                    }
                };
                (rows, false, state)
            }
            HostListing::Stale {
                rows,
                head_revision,
                since_ms,
            } => (
                rows,
                true,
                HostCoverage::Stale {
                    head_revision,
                    since_ms,
                },
            ),
        };
        rows.extend(host_rows.into_iter().take(kept).map(|row| CensusRow {
            host_id: host_id.clone(),
            stale,
            row,
        }));
        coverage.push((host_id, state));
    }
    Census { rows, coverage }
}

/// Read `host_id`'s roster through `client`, the connection the caller chose
/// for that host, and record the outcome in `registry`.
///
/// A failed read makes the host unreachable. So does a reply whose rows name
/// another host: rows are never filed under a host that did not produce
/// them, which is how a remote host could otherwise show local sessions.
pub async fn read_roster(
    registry: &mut HostRegistry,
    host_id: &HostId,
    client: &DaemonClient,
    carrier: CarrierKind,
    now_ms: i64,
) -> HostListing<RosterStatusRow> {
    let unreachable = |registry: &HostRegistry| HostListing::Unreachable {
        since_ms: match registry.get(host_id).map(|h| h.reachability) {
            Some(ainb_hangar_proto::hosts::Reachability::Unreachable { since_ms }) => since_ms,
            _ => now_ms,
        },
    };
    match client.fleet_roster_status().await {
        Ok(read) if read.rows.iter().all(|r| r.status.host_id == host_id.as_str()) => {
            registry.reached(host_id, carrier, now_ms);
            HostListing::Fresh {
                rows: read.rows,
                head_revision: read.read_revision,
            }
        }
        Ok(read) => {
            let foreign = read
                .rows
                .iter()
                .find(|r| r.status.host_id != host_id.as_str())
                .map(|r| r.status.host_id.clone());
            tracing::warn!(
                host = %host_id,
                named = ?foreign,
                "census: roster rows name another host; the listing is refused"
            );
            registry.lost(host_id);
            unreachable(registry)
        }
        Err(error) => {
            tracing::debug!(host = %host_id, %error, "census: roster read failed");
            registry.lost(host_id);
            unreachable(registry)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(n: u8) -> HostId {
        HostId::parse(&format!("01K5A0000000000000000ABC{n:02}")).expect("a ULID")
    }

    fn fresh(rows: &[&'static str]) -> HostListing<&'static str> {
        HostListing::Fresh {
            rows: rows.to_vec(),
            head_revision: 1,
        }
    }

    fn picked(census: &Census<&'static str>) -> Vec<&'static str> {
        census.rows.iter().map(|r| r.row).collect()
    }

    #[test]
    fn under_the_cap_every_host_is_covered() {
        let census = fold(
            vec![(id(1), fresh(&["a1", "a2"])), (id(2), fresh(&["b1"]))],
            10,
        );
        assert_eq!(picked(&census), ["a1", "a2", "b1"]);
        assert_eq!(census.coverage_of(&id(1)), Some(HostCoverage::Covered));
        assert_eq!(census.coverage_of(&id(2)), Some(HostCoverage::Covered));
        assert!(census.rows.iter().all(|r| !r.stale));
        assert_eq!(census.rows[2].host_id, id(2));
    }

    #[test]
    fn the_cap_is_shared_round_robin_so_a_busy_host_cannot_crowd_out_a_quiet_one() {
        let census = fold(
            vec![
                (id(1), fresh(&["a1", "a2", "a3", "a4", "a5"])),
                (id(2), fresh(&["b1", "b2"])),
                (id(3), fresh(&["c1"])),
            ],
            5,
        );
        assert_eq!(picked(&census), ["a1", "a2", "b1", "b2", "c1"]);
        assert_eq!(
            census.coverage_of(&id(1)),
            Some(HostCoverage::OmittedByCap { omitted: 3 })
        );
        assert_eq!(census.coverage_of(&id(2)), Some(HostCoverage::Covered));
        assert_eq!(census.coverage_of(&id(3)), Some(HostCoverage::Covered));

        let tight = fold(
            vec![(id(1), fresh(&["a1", "a2"])), (id(2), fresh(&["b1", "b2"]))],
            3,
        );
        assert_eq!(picked(&tight), ["a1", "a2", "b1"]);
        assert_eq!(
            tight.coverage_of(&id(2)),
            Some(HostCoverage::OmittedByCap { omitted: 1 })
        );

        let none = fold(vec![(id(1), fresh(&["a1"]))], 0);
        assert!(none.rows.is_empty());
        assert_eq!(
            none.coverage_of(&id(1)),
            Some(HostCoverage::OmittedByCap { omitted: 1 })
        );
    }

    #[test]
    fn an_unreachable_host_has_no_rows_and_says_since_when() {
        let census = fold(
            vec![
                (id(1), HostListing::Unreachable { since_ms: 77 }),
                (id(2), fresh(&["b1"])),
            ],
            10,
        );
        assert_eq!(picked(&census), ["b1"]);
        assert_eq!(
            census.coverage_of(&id(1)),
            Some(HostCoverage::Unreachable { since_ms: 77 })
        );
    }

    #[test]
    fn a_stale_host_is_never_merged_without_its_badge() {
        let stale = HostListing::Stale {
            rows: vec!["s1", "s2", "s3"],
            head_revision: 41,
            since_ms: 500,
        };
        let census = fold(vec![(id(1), fresh(&["a1", "a2", "a3"])), (id(2), stale)], 4);
        assert_eq!(picked(&census), ["a1", "a2", "s1", "s2"]);
        for row in &census.rows {
            assert_eq!(row.stale, row.host_id == id(2), "{row:?}");
        }
        assert_eq!(
            census.coverage_of(&id(2)),
            Some(HostCoverage::Stale {
                head_revision: 41,
                since_ms: 500
            }),
            "stale wins over omitted_by_cap: the badge must show"
        );
    }

    #[test]
    fn a_host_listed_twice_keeps_its_first_listing() {
        let census = fold(
            vec![(id(1), fresh(&["a1"])), (id(1), fresh(&["x1", "x2"]))],
            10,
        );
        assert_eq!(picked(&census), ["a1"]);
        assert_eq!(census.coverage.len(), 1);
    }
}
