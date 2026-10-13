use std::process::Command;

use serde_json::Value;

// CI uses the recorded, synthetic original-method corpus. Explicit opt-in reruns
// the exact private deployment oracle and fails if its version or output changed.
pub fn oracle(provider: &str, fixture: &str) -> Value {
    let frozen: Value = serde_json::from_str(fixture).expect("frozen oracle JSON");
    if std::env::var_os("JXPAN_LIVE_ORACLE").is_some() {
        let output = Command::new("node")
            .arg(format!("tools/{provider}-vectors.mjs"))
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .output()
            .expect("Node is required for live original-method verification");
        assert!(output.status.success(), "original-method oracle failed");
        let current: Value = serde_json::from_slice(&output.stdout).expect("current oracle JSON");
        assert_eq!(current, frozen, "pinned original-method corpus changed");
    }
    frozen
}
