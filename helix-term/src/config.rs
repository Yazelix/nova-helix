use crate::keymap;
use crate::keymap::{merge_keys, KeyTrie};
use helix_loader::merge_toml_values;
use helix_view::{document::Mode, theme};
use serde::Deserialize;
use std::collections::HashMap;
use std::fmt::Display;
use std::fs;
use std::io::Error as IOError;
use toml::de::Error as TomlError;

#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    pub theme: Option<theme::Config>,
    pub keys: HashMap<Mode, KeyTrie>,
    pub editor: helix_view::editor::Config,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigRaw {
    pub theme: Option<theme::Config>,
    pub keys: Option<HashMap<Mode, KeyTrie>>,
    pub editor: Option<toml::Value>,
}

impl Default for Config {
    fn default() -> Config {
        Config::load(Ok(""), Err(ConfigLoadError::default()))
            .expect("embedded default configuration must be valid")
    }
}

#[derive(Debug)]
pub enum ConfigLoadError {
    BadConfig(TomlError),
    Error(IOError),
}

impl Default for ConfigLoadError {
    fn default() -> Self {
        ConfigLoadError::Error(IOError::new(std::io::ErrorKind::NotFound, "place holder"))
    }
}

impl Display for ConfigLoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConfigLoadError::BadConfig(err) => err.fmt(f),
            ConfigLoadError::Error(err) => err.fmt(f),
        }
    }
}

impl Config {
    pub fn load(
        global: Result<&str, ConfigLoadError>,
        local: Result<String, ConfigLoadError>,
    ) -> Result<Config, ConfigLoadError> {
        let global_config: Result<ConfigRaw, ConfigLoadError> = global.and_then(|file| {
            let defaults = toml::from_str(include_str!("default_config.toml"))
                .map_err(ConfigLoadError::BadConfig)?;
            let user = toml::from_str(file).map_err(ConfigLoadError::BadConfig)?;
            merge_toml_values(defaults, user, 4)
                .try_into()
                .map_err(ConfigLoadError::BadConfig)
        });
        let local_config: Result<ConfigRaw, ConfigLoadError> =
            local.and_then(|file| toml::from_str(&file).map_err(ConfigLoadError::BadConfig));
        let res = match (global_config, local_config) {
            (Ok(global), Ok(local)) => {
                let mut keys = keymap::default();
                if let Some(global_keys) = global.keys {
                    merge_keys(&mut keys, global_keys)
                }
                if let Some(local_keys) = local.keys {
                    merge_keys(&mut keys, local_keys)
                }

                let editor = match (global.editor, local.editor) {
                    (None, None) => helix_view::editor::Config::default(),
                    (None, Some(val)) | (Some(val), None) => {
                        val.try_into().map_err(ConfigLoadError::BadConfig)?
                    }
                    (Some(global), Some(local)) => merge_toml_values(global, local, 3)
                        .try_into()
                        .map_err(ConfigLoadError::BadConfig)?,
                };

                Config {
                    theme: local.theme.or(global.theme),
                    keys,
                    editor,
                }
            }
            // if any configs are invalid return that first
            (_, Err(ConfigLoadError::BadConfig(err)))
            | (Err(ConfigLoadError::BadConfig(err)), _) => {
                return Err(ConfigLoadError::BadConfig(err))
            }
            (Ok(config), Err(_)) | (Err(_), Ok(config)) => {
                let mut keys = keymap::default();
                if let Some(keymap) = config.keys {
                    merge_keys(&mut keys, keymap);
                }
                Config {
                    theme: config.theme,
                    keys,
                    editor: config.editor.map_or_else(
                        || Ok(helix_view::editor::Config::default()),
                        |val| val.try_into().map_err(ConfigLoadError::BadConfig),
                    )?,
                }
            }

            // these are just two io errors return the one for the global config
            (Err(err), Err(_)) => return Err(err),
        };

        Ok(res)
    }

    pub fn load_default() -> Result<Config, ConfigLoadError> {
        let global_config = match fs::read_to_string(helix_loader::config_file()) {
            Ok(config) => config,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(err) => return Err(ConfigLoadError::Error(err)),
        };
        let local_config = fs::read_to_string(helix_loader::workspace_config_file())
            .map_err(ConfigLoadError::Error);

        let phony_config = ConfigLoadError::Error(IOError::other("hacky placeholder"));
        let global_parsed = Config::load(Ok(&global_config), Err(phony_config))?;

        // We need to build a transient `WorkspaceTrust` just to ask whether the workspace is
        // trusted enough to load its `.helix/config.toml`. The persisted-trust file on disk is the
        // source of truth either way; this transient instance has an empty cache and is dropped
        // after the check.
        let trust = helix_loader::workspace_trust::WorkspaceTrust::new(
            (&global_parsed.editor.workspace_trust).into(),
        );
        if trust
            .query_current(helix_loader::workspace_trust::TrustQuery::LocalConfig)
            .is_trusted()
        {
            let mut merged = Config::load(Ok(&global_config), local_config)?;
            // editor.workspace-trust is global/user-scope only. Without this override, a
            // workspace's `.helix/config.toml` could set `level = "insecure"`; once the user trusted
            // *that* workspace, refresh_config would re-load with the override merged in and from
            // then on every subsequent workspace in the session would be implicitly trusted. Pin
            // the gate's own configuration to the global file.
            merged.editor.workspace_trust = global_parsed.editor.workspace_trust;
            Ok(merged)
        } else {
            Ok(global_parsed)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    impl Config {
        fn load_test(config: &str) -> Config {
            Config::load(Ok(config), Err(ConfigLoadError::default())).unwrap()
        }
    }

    #[test]
    fn parsing_keymaps_config_file() {
        use crate::keymap;
        use helix_core::hashmap;
        use helix_view::document::Mode;

        let sample_keymaps = r#"
            [keys.insert]
            y = "move_line_down"
            S-C-a = "delete_selection"

            [keys.normal]
            A-F12 = "move_next_word_end"
        "#;

        let mut keys = keymap::default();
        merge_keys(
            &mut keys,
            hashmap! {
                Mode::Insert => keymap!({ "Insert mode"
                    "y" => move_line_down,
                    "S-C-a" => delete_selection,
                }),
                Mode::Normal => keymap!({ "Normal mode"
                    "A-F12" => move_next_word_end,
                }),
            },
        );

        assert_eq!(
            Config::load_test(sample_keymaps),
            Config {
                keys,
                ..Default::default()
            }
        );
    }

    #[test]
    fn keys_resolve_to_correct_defaults() {
        // From serde default
        let default_keys = Config::load_test("").keys;
        assert_eq!(default_keys, keymap::default());

        // From the Default trait
        let default_keys = Config::default().keys;
        assert_eq!(default_keys, keymap::default());
    }

    #[test]
    fn embedded_defaults_apply_to_empty_config_and_error_fallback() {
        let embedded = Config::load_test(include_str!("default_config.toml"));
        assert!(Config::load_test("") == embedded);
        // Startup and health use Default after reporting malformed user configuration.
        assert!(Config::default() == embedded);
    }

    #[test]
    fn user_overrides_preserve_unspecified_embedded_defaults() {
        let embedded = Config::load_test(include_str!("default_config.toml"));
        let user = Config::load_test(
            r#"
            theme = "default"
            [editor]
            cursorline = false
            [editor.soft-wrap]
            max-wrap = 10
            "#,
        );
        assert_ne!(user.theme, embedded.theme);
        assert!(!user.editor.cursorline);
        assert_eq!(user.editor.soft_wrap.max_wrap, Some(10));
        assert_eq!(
            user.editor.soft_wrap.enable,
            embedded.editor.soft_wrap.enable
        );
        assert_eq!(user.editor.bufferline, embedded.editor.bufferline);
        assert_eq!(user.keys, keymap::default());
    }

    #[test]
    fn workspace_overrides_user_and_embedded_config() {
        let workspace =
            "[editor]\ncursorline = false\nbufferline = 'never'\n[editor.soft-wrap]\nmax-wrap = 20";
        for user in ["", "theme = 'default'\n[editor]\ncursorline = true"] {
            let merged = Config::load(Ok(user), Ok(workspace.to_owned())).unwrap();
            assert_eq!(merged.theme, Config::load_test(user).theme);
            assert!(!merged.editor.cursorline);
            assert_eq!(merged.editor.soft_wrap.max_wrap, Some(20));
            assert_eq!(
                merged.editor.bufferline,
                helix_view::editor::BufferLine::Never
            );
        }
    }

    #[test]
    fn invalid_user_and_workspace_configs_remain_errors() {
        for invalid in ["[", "[editor]\ncursorline = 1", "unknown = true"] {
            assert!(matches!(
                Config::load(Ok(invalid), Err(ConfigLoadError::default())),
                Err(ConfigLoadError::BadConfig(_))
            ));
            assert!(matches!(
                Config::load(Ok(""), Ok(invalid.to_owned())),
                Err(ConfigLoadError::BadConfig(_))
            ));
        }
    }
}
