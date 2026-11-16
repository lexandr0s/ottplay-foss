//! Server-owned multiview authorization. Not a production capability declaration.
//!
//! Native selection witnesses and favorite digests cannot enroll a source. Only
//! separately authorized server configuration can establish upstream authority.
mod hls_timeline;
pub mod network;
pub mod registry;
pub mod resolvers;
pub mod runtime;
pub mod stream;
mod stream_hls;
pub mod stream_transport;
pub mod wire;
