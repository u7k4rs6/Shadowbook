//! The proptest runner: small cases (~200 commands), for shrinking.
//!
//! Each case is a `Vec<u64>` of per-step seeds, not a `Vec<Command>`
//! directly -- the generator is stateful (every command it emits depends
//! on its shadow book's current live/retired state), which does not fit
//! a pure, execution-independent `Strategy`. Instead, proptest shrinks
//! the raw per-step entropy (removing steps, simplifying seed values
//! toward 0), and each step seed is fed to a fresh, small `StdRng` that
//! the SAME `Generator::next_command` used by the seeded volume runner
//! consumes deterministically. A shrunk seed sequence, replayed through
//! that same deterministic interpretation, still produces a valid,
//! smaller reproducing case.

use proptest::prelude::*;
use rand::rngs::StdRng;
use rand::SeedableRng;

use fuzz::{Generator, Harness, FUZZ_CONFIG};

proptest! {
    #[test]
    fn differential_holds_over_random_command_sequences(steps in prop::collection::vec(any::<u64>(), 1..=200)) {
        let mut generator = Generator::new(FUZZ_CONFIG);
        let mut harness = Harness::new(FUZZ_CONFIG);

        for step_seed in steps {
            let mut step_rng = StdRng::seed_from_u64(step_seed);
            let cmd = generator.next_command(&mut step_rng);
            harness.apply(cmd).expect("differential or invariant failure");
        }

        harness.check_replay().expect("replay determinism failure");
    }
}
