//! Agents a person can hand an issue to (ADR-0001, `report_8ff3aa928be0`).
//!
//! A bot account becomes an agent through its [`profile`]: what it runs, on
//! which runner labels, with whose model key, in which repositories and under
//! which limits. An assignment to it starts a [`run`]: the checks of D6, the
//! run's record and its synthetic pipeline. The runner side comes in later
//! cards of `phase_948358cdfcf6`.

pub mod profile;
pub mod run;
pub mod trust;
