//! The pieces that turn the pure engine into a running process: the
//! journal it is replayed from, the timers it asks for, and the admin hub
//! it reports to.

pub mod admin;
pub mod group;
pub mod journal;
pub mod model;
pub mod redact;
pub mod router;
pub mod timers;

/// Days the action log is kept per kind (DESIGN.md section 9).
pub fn retention_days(kind: ours_types::Kind) -> u32 {
    use ours_types::Kind;
    match kind {
        Kind::Team | Kind::Project | Kind::Committee => 365,
        _ => 90,
    }
}
