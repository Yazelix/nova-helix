use std::process::Command;

#[test]
fn listen_flag_is_parsed_and_documented() {
    let output = Command::new(env!("CARGO_BIN_EXE_hx"))
        .args(["--listen", "/private/editor.sock", "--help"])
        .output()
        .unwrap();
    #[cfg(unix)]
    {
        assert!(output.status.success());
        assert!(String::from_utf8_lossy(&output.stdout).contains("--listen <path>"));
    }
    #[cfg(not(unix))]
    {
        assert!(!output.status.success());
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("--listen is only available on Unix")
        );
    }
}

#[cfg(unix)]
#[test]
fn listen_requires_a_path() {
    let output = Command::new(env!("CARGO_BIN_EXE_hx"))
        .arg("--listen")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr)
        .contains("--listen must specify an absolute socket path"));
}
