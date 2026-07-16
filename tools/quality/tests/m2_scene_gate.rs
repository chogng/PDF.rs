mod m2_scene_gate_support;

#[test]
fn normative_m2_scene_gate_is_exact_and_replay_stable() {
    m2_scene_gate_support::run_gate();
}
