//! The `if:` evaluator lives in `rg-process`, where the external runner can
//! reach it too: a step's condition is read by whichever runner runs the step.

pub use rg_process::ci_condition::*;
