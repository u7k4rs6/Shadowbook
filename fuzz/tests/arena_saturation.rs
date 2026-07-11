//! A targeted supplement to the main balanced run, not a replacement for
//! it. The balanced distribution (`CommandMix::BALANCED`, New 60% /
//! Cancel 25% / Amend 15%) never once drove the book to
//! `Config::capacity` in 10^8 operations: Cancel succeeds often enough
//! (~56%) that the book stays thin, so `RejectReason::ArenaFull` -- a
//! central mechanism of this project -- got zero fuzz coverage from that
//! run, only unit coverage.
//!
//! `CommandMix::SATURATING` (New 80% / Cancel 10% / Amend 10%) starves
//! Cancel and biases hard toward New, so resting orders accumulate
//! faster than they're removed and the arena genuinely fills. Bounded to
//! 10^6 operations: this run exists to reach a specific state, not to be
//! a second volume instrument.

use rand::rngs::StdRng;
use rand::SeedableRng;

use fuzz::{CommandMix, Generator, Harness, FUZZ_CONFIG};
use types::RejectReason;

#[test]
fn arena_full_fires_under_a_saturating_distribution() {
    let mut rng = StdRng::seed_from_u64(7);
    // A wide id pool, not just a New-heavy mix: see `with_mix_and_pool`'s
    // doc comment for why the balanced mix's narrow (~2x capacity) pool
    // is self-limiting for a saturating distribution specifically.
    let pool_size = FUZZ_CONFIG.capacity as u64 * 64;
    let mut generator = Generator::with_mix_and_pool(FUZZ_CONFIG, CommandMix::SATURATING, pool_size);
    let mut harness = Harness::new(FUZZ_CONFIG);

    let operations = 1_000_000u64;
    for i in 0..operations {
        let cmd = generator.next_command(&mut rng);
        if let Err(failure) = harness.apply(cmd) {
            panic!("divergence or invariant violation at operation {i}: {failure:#?}");
        }
    }

    harness.check_replay().expect("replay determinism failure");

    let arena_full_count = generator.stats.new_rejects.get(&RejectReason::ArenaFull).copied().unwrap_or(0);
    println!("ArenaFull count under saturating distribution: {arena_full_count}");
    println!("{}", generator.stats.report());
    assert!(
        arena_full_count > 0,
        "ArenaFull never fired under a New-saturating distribution -- the arena capacity path still has no fuzz coverage"
    );
}
