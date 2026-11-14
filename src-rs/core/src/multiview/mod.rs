//! Composition primitives and an explicitly configured private media host.
//!
//! Ordinary playback and unconfigured servers do not enable multiview. The
//! protected operator plan enrolls server-owned sources and selects the admitted
//! production profile; fixture constructors retain their separate test limits.
//! See the broker runbook for authority, retirement and qualification boundaries.

pub mod broker;
pub mod decoder;
pub mod encoder;
pub mod frames;
pub mod geometry;
pub mod persistent;
pub mod plan;
mod timing;
pub mod worker;

#[cfg(test)]
mod tests;
