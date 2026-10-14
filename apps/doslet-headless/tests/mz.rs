use std::{fs, process::Command};

include!("../../../tests/fixtures/relocated_mz.rs");

#[test]
fn run_mz_executes_a_file_and_reports_budget_exhaustion() {
    let path = std::env::temp_dir().join(format!("doslet-mz-{}.exe", std::process::id()));
    fs::write(&path, relocated_mz()).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_doslet-headless"))
        .arg("run-mz")
        .arg(&path)
        .args(["--steps", "100"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("Status: Exited(7)"));
    assert!(stdout.contains("DOS output: Relocated MZ"));
    let exhausted = Command::new(env!("CARGO_BIN_EXE_doslet-headless"))
        .arg("run-mz")
        .arg(&path)
        .args(["--steps", "3"])
        .output()
        .unwrap();
    fs::remove_file(&path).unwrap();
    assert!(!exhausted.status.success());
    assert!(
        String::from_utf8(exhausted.stderr)
            .unwrap()
            .contains("execution step budget exhausted")
    );
}
