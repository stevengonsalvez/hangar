// ABOUTME: The hosts a surface can see (R1-11): the local daemon and every paired
// one, how reachable each is, and the census that folds their listings into one
// capped list without ever hiding which host a row came from or how fresh it is.
//
//   HostRegistry ──reached / lost / behind──▶ HostApp { reachability }
//        │
//        ▼ one HostListing per host (fresh rows, stale rows, or none)
//   census::fold(listings, cap) ──▶ Census { rows (host-tagged), coverage }
//
// Nothing here dials a host or starts a task. The registry is data, the fold
// is a pure function, and `census::read_roster` is the one async edge: it reads
// `fleet/roster_status` from a client the caller already chose for that host,
// so a remote host is never read through the local socket by accident.

pub mod census;
pub mod registry;

pub use ainb_hangar_proto::hosts::{CarrierKind, HostCoverage, HostId, Reachability};
pub use census::{Census, CensusRow, HostListing, fold, read_roster};
pub use registry::{HostApp, HostKind, HostRegistry};
