// ABOUTME: The hosts a surface can see (R1-11): the local daemon and every paired
// one, how reachable each is, and the census that folds their listings into one
// capped list without ever hiding which host a row came from or how fresh it is.
//
//   HostRegistry ──reached / lost / behind──▶ HostApp { reachability }
//        │
//        ▼ one HostListing per host (fresh rows, stale rows, or none)
//   census::fold(listings, cap) ──▶ Census { rows (host-tagged), coverage }
//
// Nothing here dials a host, reads a daemon or starts a task. The registry is
// data, the fold is a pure function, and `census::listing_from_read` files a
// `fleet/roster_status` read the caller already took (through the one agent
// status reader, #1188, on the connection it chose for that host).

pub mod census;
pub mod registry;

pub use ainb_hangar_proto::hosts::{CarrierKind, HostCoverage, HostId, Reachability};
pub use census::{Census, CensusRow, HostListing, fold, listing_from_read};
pub use registry::{HostApp, HostKind, HostRegistry, MAX_PAIRED_HOSTS, RegistryError};
