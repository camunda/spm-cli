//! A thin wrapper over the system `git` binary (no libgit2), split by concern:
//! [`command`] spawns git, [`transport`] decides how a remote is contacted, and
//! [`repository`] holds the operations callers use. The public API is
//! re-exported here, so callers only ever name `git::…`.

mod command;
mod repository;
mod transport;

pub use command::version;
pub use repository::{
    default_branch, fetch_commit, is_at_commit, is_ignored, is_tracked, ls_remote, probe,
    remote_ref_names, remote_refs, Reach,
};
pub use transport::{contacts_over_ssh, remote_identity, set_transport, Protocol, Transport};
