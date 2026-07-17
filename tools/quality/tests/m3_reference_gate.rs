mod m3_reference_gate_support;

#[test]
fn strict_full_native_reference_gate_is_atomic_and_replay_stable() {
    m3_reference_gate_support::run_gate();
}
