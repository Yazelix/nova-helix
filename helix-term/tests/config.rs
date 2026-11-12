use helix_term::config::{Config, ConfigLoadError};

#[test]
fn missing_user_file_uses_embedded_defaults_but_other_read_errors_fail() {
    let root = tempfile::tempdir().unwrap();
    let config = root.path().join("config.toml");
    helix_loader::initialize_config_file(Some(config.clone()));

    assert!(Config::load_default().unwrap() == Config::default());
    assert!(!config.exists());

    std::fs::create_dir(&config).unwrap();
    assert!(matches!(
        Config::load_default(),
        Err(ConfigLoadError::Error(err)) if err.kind() != std::io::ErrorKind::NotFound
    ));
}
