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
        Config {
            theme: None,
            keys: keymap::default(),
            editor: helix_view::editor::Config::default(),
        }
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
        global: Result<&String, ConfigLoadError>,
        local: Result<String, ConfigLoadError>,
    ) -> Result<Config, ConfigLoadError> {
        let global_config: Result<ConfigRaw, ConfigLoadError> =
            global.and_then(|file| toml::from_str(file).map_err(ConfigLoadError::BadConfig));
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
        let global_config =
            fs::read_to_string(helix_loader::config_file()).map_err(ConfigLoadError::Error)?;
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
            Config::load(Ok(&config.to_owned()), Err(ConfigLoadError::default())).unwrap()
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
    fn parsing_breadcrumbs_config() {
        use helix_view::editor::{BreadcrumbsConfig, StatusLineElement};

        let breadcrumbs = |config: &str| Config::load_test(config).editor.statusline.breadcrumbs;

        assert_eq!(breadcrumbs(""), BreadcrumbsConfig::default());
        assert_eq!(
            breadcrumbs(
                "[editor.statusline.breadcrumbs]\nseparator = \"›\"\nleading-separator = false\ntruncate = false"
            ),
            BreadcrumbsConfig {
                separator: "›".to_string(),
                leading_separator: false,
                truncate: false,
            }
        );

        // The example in the book
        let statusline = Config::load_test(
            r#"
            [editor.statusline]
            left = ["mode", "spinner", "file-name", "read-only-indicator", "file-modification-indicator"]
            right = ["breadcrumbs", "diagnostics", "selections", "register", "position", "file-encoding"]

            [editor.statusline.breadcrumbs]
            leading-separator = false
            "#,
        )
        .editor
        .statusline;
        assert_eq!(statusline.right[0], StatusLineElement::Breadcrumbs);
        assert!(!statusline.breadcrumbs.leading_separator);

        let typo = "[editor.statusline.breadcrumbs]\nleading-separators = false".to_owned();
        assert!(Config::load(Ok(&typo), Err(ConfigLoadError::default())).is_err());
    }

    #[test]
    fn parsing_smooth_scroll_config() {
        use helix_view::editor::SmoothScrollConfig;
        use std::time::Duration;

        let smooth_scroll = |config: &str| Config::load_test(config).editor.smooth_scroll;

        assert_eq!(smooth_scroll(""), SmoothScrollConfig::default());
        assert_eq!(
            smooth_scroll("[editor]\nsmooth-scroll = true"),
            SmoothScrollConfig {
                enable: true,
                ..Default::default()
            }
        );
        assert_eq!(
            smooth_scroll(
                "[editor.smooth-scroll]\nenable = true\nduration = 90\nhide-cursor = true"
            ),
            SmoothScrollConfig {
                enable: true,
                duration: Duration::from_millis(90),
                hide_cursor: true,
            }
        );
        assert_eq!(
            smooth_scroll("[editor.smooth-scroll]\nhide-cursor = true"),
            SmoothScrollConfig {
                hide_cursor: true,
                ..Default::default()
            }
        );

        let typo = "[editor.smooth-scroll]\nenabled = true".to_owned();
        assert!(Config::load(Ok(&typo), Err(ConfigLoadError::default())).is_err());
    }

    #[test]
    fn parsing_folding_config() {
        use helix_view::editor::FoldingConfig;

        let folding = |config: &str| Config::load_test(config).editor.folding;

        assert_eq!(folding(""), FoldingConfig::default());
        assert_eq!(
            folding("[editor.folding]\nstart-folded = true\nplaceholder = \"⋯\""),
            FoldingConfig {
                start_folded: true,
                placeholder: '⋯',
            }
        );

        for invalid in [
            "[editor.folding]\nstart-fold = true",
            "[editor.folding]\nplaceholder = \"...\"",
            "[editor.folding]\nplaceholder = \"\\n\"",
            "[editor.folding]\nplaceholder = \"\\t\"",
        ] {
            let invalid = invalid.to_owned();
            assert!(
                Config::load(Ok(&invalid), Err(ConfigLoadError::default())).is_err(),
                "{invalid}"
            );
        }
    }

    #[test]
    fn parsing_file_tree_config() {
        use helix_view::editor::{
            FileTreeConfig, FileTreeSide, FileTreeSort, FileTreeStart, LsColors,
        };

        let file_tree = |config: &str| Config::load_test(config).editor.file_tree;

        assert_eq!(file_tree(""), FileTreeConfig::default());
        assert_eq!(
            file_tree(
                "[editor.file-tree]\nstart = \"multiple\"\nside = \"right\"\nicons = false\n\
                 guides = false\nflatten-dirs = false\nsort = \"alphabetical\"\nls-colors = true"
            ),
            FileTreeConfig {
                start: FileTreeStart::Multiple,
                side: FileTreeSide::Right,
                icons: false,
                guides: false,
                flatten_dirs: false,
                sort: FileTreeSort::Alphabetical,
                ls_colors: LsColors::Environment(true),
            }
        );
        assert_eq!(
            file_tree("[editor.file-tree]\nls-colors = \"di=1;34:*.rs=33\"").ls_colors,
            LsColors::Spec("di=1;34:*.rs=33".to_owned())
        );

        for invalid in [
            "[editor.file-tree]\nside = \"middle\"",
            "[editor.file-tree]\nstart = \"sometimes\"",
            "[editor.file-tree]\nls-colors = 1",
            "[editor.file-tree]\nwidth = 30",
        ] {
            let invalid = invalid.to_owned();
            assert!(
                Config::load(Ok(&invalid), Err(ConfigLoadError::default())).is_err(),
                "{invalid}"
            );
        }
    }

    #[test]
    fn set_smooth_scroll_shorthand() {
        // `:set smooth-scroll true` replaces the serialized table with a boolean
        let mut config = serde_json::json!(helix_view::editor::Config::default());
        *config.pointer_mut("/smooth-scroll").unwrap() = serde_json::Value::Bool(true);
        let config: helix_view::editor::Config = serde_json::from_value(config).unwrap();
        assert!(config.smooth_scroll.enable);
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
}
