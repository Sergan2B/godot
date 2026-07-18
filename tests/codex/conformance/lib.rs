#![deny(unsafe_code)]

#[cfg(any(unix, test))]
mod bundle;
#[cfg(unix)]
mod discovery;
mod error;
#[cfg(any(unix, test))]
mod json;
#[cfg(any(unix, test))]
mod protocol;
#[cfg(unix)]
mod suite;
#[cfg(unix)]
mod trace;

pub use error::{ConformanceError, Result};
#[cfg(unix)]
pub use suite::{RunOptions, RunSummary, run};
