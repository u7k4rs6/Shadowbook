//! The differential harness: applies one command stream to both engines,
//! asserting event equality and all nine invariants after every single
//! command, not just at the end of a run. Checking only final book state
//! is the mistake that lets ordering bugs through, and ordering is what a
//! matching engine sells.

use engine::Book as FastBook;
use reference::RefBook as SlowBook;
use types::{Command, Config, Event};

#[derive(Debug)]
pub struct Divergence {
    pub index: usize,
    pub cmd: Command,
    pub fast_events: Vec<Event>,
    pub slow_events: Vec<Event>,
}

#[derive(Debug)]
pub enum Failure {
    /// The two engines disagree on what a command did. This is the
    /// differential's own reason to exist.
    EventDivergence(Divergence),
    /// A snapshot invariant (I1, I5, I6, I7, I8, only-resting-kinds)
    /// failed against the optimized engine's own state, independent of
    /// what the reference engine did. Catches a bug present in `engine`
    /// alone, or (with `SlowInvariant`) one present identically in both,
    /// which the differential comparison above can never see.
    FastInvariant { index: usize, cmd: Command, violations: Vec<String> },
    SlowInvariant { index: usize, cmd: Command, violations: Vec<String> },
    /// I2/I3/I4, checked against each engine's own emitted event stream
    /// independently via `types::EventAuditor`.
    FastAuditor { index: usize, cmd: Command, violations: Vec<String> },
    SlowAuditor { index: usize, cmd: Command, violations: Vec<String> },
    /// I9: a fresh engine fed the same command log did not reach a
    /// byte-identical digest.
    ReplayMismatch { which: &'static str },
}

pub struct Harness {
    pub fast: FastBook,
    pub slow: SlowBook,
    cfg: Config,
    fast_auditor: types::EventAuditor,
    slow_auditor: types::EventAuditor,
}

impl Harness {
    pub fn new(cfg: Config) -> Self {
        Harness {
            fast: FastBook::new(cfg),
            slow: SlowBook::new(cfg),
            cfg,
            fast_auditor: types::EventAuditor::new(),
            slow_auditor: types::EventAuditor::new(),
        }
    }

    /// `slow` (a `RefBook`) already keeps its own internal command log
    /// for `command_log`/`replay_matches`; the harness reuses that one
    /// rather than keeping a second, identical, multi-gigabyte-at-fuzz-
    /// scale copy of its own. Both engines receive the exact same
    /// command in `apply`, so `slow`'s log is authoritative for `fast`'s
    /// replay too.
    pub fn command_count(&self) -> usize {
        self.slow.command_log().len()
    }

    /// Applies `cmd` to both engines. Three checks, in order, per
    /// command: event-stream equality (elementwise, not just final
    /// state), then all nine invariants against each engine
    /// independently (I2/I3/I4 via each engine's own auditor; I8 is only
    /// meaningful for `fast`, which is what `engine::check_invariants`
    /// checks and `reference::check_invariants` deliberately does not).
    pub fn apply(&mut self, cmd: Command) -> Result<(), Failure> {
        let index = self.command_count();

        let fast_events = self.fast.apply(cmd).to_vec();
        let slow_events = self.slow.apply(cmd);

        if fast_events != slow_events {
            return Err(Failure::EventDivergence(Divergence { index, cmd, fast_events, slow_events }));
        }

        self.fast_auditor.observe(&cmd, &fast_events);
        self.slow_auditor.observe(&cmd, &slow_events);

        let fast_violations = engine::check_invariants(&self.fast);
        if !fast_violations.is_empty() {
            return Err(Failure::FastInvariant { index, cmd, violations: fast_violations });
        }
        let slow_violations = reference::check_invariants(&self.slow);
        if !slow_violations.is_empty() {
            return Err(Failure::SlowInvariant { index, cmd, violations: slow_violations });
        }
        if !self.fast_auditor.violations.is_empty() {
            return Err(Failure::FastAuditor { index, cmd, violations: self.fast_auditor.violations.clone() });
        }
        if !self.slow_auditor.violations.is_empty() {
            return Err(Failure::SlowAuditor { index, cmd, violations: self.slow_auditor.violations.clone() });
        }

        Ok(())
    }

    /// I9, checked against both engines: a fresh instance fed the exact
    /// command log this harness has seen so far must reach a
    /// byte-identical digest.
    pub fn check_replay(&self) -> Result<(), Failure> {
        let mut fresh_fast = FastBook::new(self.cfg);
        for &cmd in self.slow.command_log() {
            fresh_fast.apply(cmd);
        }
        if fresh_fast.digest() != self.fast.digest() {
            return Err(Failure::ReplayMismatch { which: "engine" });
        }
        if !self.slow.replay_matches() {
            return Err(Failure::ReplayMismatch { which: "reference" });
        }
        Ok(())
    }
}
