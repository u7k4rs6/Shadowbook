//! `benches`: tail latency under adversarial load, at production scale
//! (`types::BENCH_CONFIG`). The bench target itself
//! (`benches/matching.rs`) does the work; this crate exists only to give
//! it somewhere to live as a workspace member.

#![forbid(unsafe_code)]
