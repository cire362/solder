//! How long a test waits for something a real process does.
//!
//! A shell, a language server or a debug adapter answers in a few
//! milliseconds here and in seconds on a runner that runs three hundred
//! tests at once on two cores. A wait ends as soon as what it waits for
//! is there, so a long one costs a passing run nothing: it only decides
//! how slow a machine may be before a test that works is called broken.

use std::time::Duration;

pub const WAIT: Duration = Duration::from_secs(30);

/// The same wait for the loops that look every ten milliseconds.
pub const LOOKS: usize = 3000;
