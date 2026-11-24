//! Domain model for Kiln. No UI and no I/O: everything here is plain data
//! so the privacy policy can be tested without a window.

pub mod persist;
pub mod policy;
pub mod store;

pub use store::{Agent, Approval, Message, Project, Role, Status, Store, Thread, NEW_THREAD_TITLE};
