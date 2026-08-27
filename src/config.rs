use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Server configuration root.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Config {
    /// Experimental feature configurations.
    #[serde(default)]
    pub experimental: ExperimentalConfig,
}

/// Experimental feature settings.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExperimentalConfig {
    /// Whether experimental syntax diagnostics using `zsh -n` is enabled.
    #[serde(default)]
    pub diagnostics: bool,
    /// Whether experimental hover documentation provider is enabled.
    #[serde(default)]
    pub hover: bool,
    /// Whether experimental definition provider is enabled.
    #[serde(default)]
    pub definition: bool,
}

impl Config {
    /// Parses configuration from an optional JSON value (e.g. `initializationOptions` or `settings`).
    ///
    /// Looks for `["zshcs"]["experimental"]["diagnostics"]`, `["zshcs"]["experimental"]["hover"]`,
    /// and `["zshcs"]["experimental"]["definition"]` first,
    /// then `["settings"]["zshcs"]...`, and falls back to `["experimental"]...` if present.
    /// Defaults to `false` for experimental features.
    pub fn from_value(value: Option<&Value>) -> Self {
        let Some(val) = value else {
            return Self::default();
        };

        if val.is_null() {
            return Self::default();
        }

        // 1. Try parsing from `zshcs` key: {"zshcs": {"experimental": {"diagnostics": true, "hover": true, "definition": true}}}
        if let Some(zshcs_val) = val.get("zshcs")
            && let Ok(config) = serde_json::from_value::<Config>(zshcs_val.clone())
        {
            return config;
        }

        // 2. Try unwrapping `settings` wrapper: {"settings": {"zshcs": ...}}
        if let Some(settings_val) = val.get("settings") {
            return Self::from_value(Some(settings_val));
        }

        // 3. Try parsing root object directly: {"experimental": {"diagnostics": true, "hover": true, "definition": true}}
        if let Ok(config) = serde_json::from_value::<Config>(val.clone()) {
            return config;
        }

        Self::default()
    }

    /// Returns true if experimental diagnostics is enabled.
    pub fn experimental_diagnostics(&self) -> bool {
        self.experimental.diagnostics
    }

    /// Returns true if experimental hover is enabled.
    pub fn experimental_hover(&self) -> bool {
        self.experimental.hover
    }

    /// Returns true if experimental definition is enabled.
    pub fn experimental_definition(&self) -> bool {
        self.experimental.definition
    }
}

/// Extracts whether experimental diagnostics is enabled from an optional JSON value.
pub fn extract_experimental_diagnostics(value: Option<&Value>) -> bool {
    Config::from_value(value).experimental_diagnostics()
}

/// Extracts whether experimental hover is enabled from an optional JSON value.
pub fn extract_experimental_hover(value: Option<&Value>) -> bool {
    Config::from_value(value).experimental_hover()
}

/// Extracts whether experimental definition is enabled from an optional JSON value.
pub fn extract_experimental_definition(value: Option<&Value>) -> bool {
    Config::from_value(value).experimental_definition()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_config_default() {
        let config = Config::default();
        assert!(!config.experimental.diagnostics);
        assert!(!config.experimental_diagnostics());
        assert!(!config.experimental.hover);
        assert!(!config.experimental_hover());
        assert!(!config.experimental.definition);
        assert!(!config.experimental_definition());
    }

    #[test]
    fn test_extract_diagnostics_none() {
        assert!(!extract_experimental_diagnostics(None));
    }

    #[test]
    fn test_extract_diagnostics_null() {
        let val = json!(null);
        assert!(!extract_experimental_diagnostics(Some(&val)));
    }

    #[test]
    fn test_extract_diagnostics_empty_object() {
        let val = json!({});
        assert!(!extract_experimental_diagnostics(Some(&val)));
    }

    #[test]
    fn test_extract_diagnostics_nested_zshcs_true() {
        let val = json!({
            "zshcs": {
                "experimental": {
                    "diagnostics": true
                }
            }
        });
        let config = Config::from_value(Some(&val));
        assert!(config.experimental_diagnostics());
        assert!(extract_experimental_diagnostics(Some(&val)));
    }

    #[test]
    fn test_extract_diagnostics_nested_zshcs_false() {
        let val = json!({
            "zshcs": {
                "experimental": {
                    "diagnostics": false
                }
            }
        });
        let config = Config::from_value(Some(&val));
        assert!(!config.experimental_diagnostics());
        assert!(!extract_experimental_diagnostics(Some(&val)));
    }

    #[test]
    fn test_extract_diagnostics_direct_experimental_true() {
        let val = json!({
            "experimental": {
                "diagnostics": true
            }
        });
        let config = Config::from_value(Some(&val));
        assert!(config.experimental_diagnostics());
        assert!(extract_experimental_diagnostics(Some(&val)));
    }

    #[test]
    fn test_extract_diagnostics_direct_experimental_false() {
        let val = json!({
            "experimental": {
                "diagnostics": false
            }
        });
        let config = Config::from_value(Some(&val));
        assert!(!config.experimental_diagnostics());
        assert!(!extract_experimental_diagnostics(Some(&val)));
    }

    #[test]
    fn test_extract_diagnostics_invalid_types() {
        let val1 = json!("invalid string");
        assert!(!extract_experimental_diagnostics(Some(&val1)));

        let val2 = json!(12345);
        assert!(!extract_experimental_diagnostics(Some(&val2)));

        let val3 = json!({
            "zshcs": "invalid string value"
        });
        assert!(!extract_experimental_diagnostics(Some(&val3)));

        let val4 = json!({
            "experimental": "invalid string value"
        });
        assert!(!extract_experimental_diagnostics(Some(&val4)));

        let val5 = json!({
            "zshcs": {
                "experimental": {
                    "diagnostics": "not a boolean"
                }
            }
        });
        assert!(!extract_experimental_diagnostics(Some(&val5)));
    }

    #[test]
    fn test_extract_diagnostics_nested_settings_wrapper() {
        let val = json!({
            "settings": {
                "zshcs": {
                    "experimental": {
                        "diagnostics": true
                    }
                }
            }
        });
        let config = Config::from_value(Some(&val));
        assert!(config.experimental_diagnostics());
        assert!(extract_experimental_diagnostics(Some(&val)));

        let val_false = json!({
            "settings": {
                "zshcs": {
                    "experimental": {
                        "diagnostics": false
                    }
                }
            }
        });
        let config_false = Config::from_value(Some(&val_false));
        assert!(!config_false.experimental_diagnostics());
        assert!(!extract_experimental_diagnostics(Some(&val_false)));

        let val_direct = json!({
            "settings": {
                "experimental": {
                    "diagnostics": true
                }
            }
        });
        let config_direct = Config::from_value(Some(&val_direct));
        assert!(config_direct.experimental_diagnostics());
        assert!(extract_experimental_diagnostics(Some(&val_direct)));
    }

    #[test]
    fn test_config_serialization_roundtrip() {
        let config = Config {
            experimental: ExperimentalConfig {
                diagnostics: true,
                hover: true,
                definition: true,
            },
        };
        let val = serde_json::to_value(&config).unwrap();
        assert_eq!(
            val,
            json!({ "experimental": { "diagnostics": true, "hover": true, "definition": true } })
        );

        let deserialized = Config::from_value(Some(&val));
        assert_eq!(config, deserialized);
    }

    #[test]
    fn test_extract_hover_none_and_null() {
        assert!(!extract_experimental_hover(None));
        let val_null = json!(null);
        assert!(!extract_experimental_hover(Some(&val_null)));
        let val_empty = json!({});
        assert!(!extract_experimental_hover(Some(&val_empty)));
    }

    #[test]
    fn test_extract_hover_nested_zshcs() {
        let val = json!({
            "zshcs": {
                "experimental": {
                    "hover": true
                }
            }
        });
        let config = Config::from_value(Some(&val));
        assert!(config.experimental_hover());
        assert!(extract_experimental_hover(Some(&val)));

        let val_false = json!({
            "zshcs": {
                "experimental": {
                    "hover": false
                }
            }
        });
        let config_false = Config::from_value(Some(&val_false));
        assert!(!config_false.experimental_hover());
        assert!(!extract_experimental_hover(Some(&val_false)));
    }

    #[test]
    fn test_extract_hover_direct_experimental() {
        let val = json!({
            "experimental": {
                "hover": true
            }
        });
        let config = Config::from_value(Some(&val));
        assert!(config.experimental_hover());
        assert!(extract_experimental_hover(Some(&val)));

        let val_false = json!({
            "experimental": {
                "hover": false
            }
        });
        let config_false = Config::from_value(Some(&val_false));
        assert!(!config_false.experimental_hover());
        assert!(!extract_experimental_hover(Some(&val_false)));
    }

    #[test]
    fn test_extract_hover_nested_settings_wrapper() {
        let val = json!({
            "settings": {
                "zshcs": {
                    "experimental": {
                        "hover": true
                    }
                }
            }
        });
        let config = Config::from_value(Some(&val));
        assert!(config.experimental_hover());
        assert!(extract_experimental_hover(Some(&val)));

        let val_false = json!({
            "settings": {
                "zshcs": {
                    "experimental": {
                        "hover": false
                    }
                }
            }
        });
        let config_false = Config::from_value(Some(&val_false));
        assert!(!config_false.experimental_hover());
        assert!(!extract_experimental_hover(Some(&val_false)));
    }

    #[test]
    fn test_extract_hover_invalid_types() {
        let val1 = json!("invalid string");
        assert!(!extract_experimental_hover(Some(&val1)));

        let val2 = json!(999);
        assert!(!extract_experimental_hover(Some(&val2)));

        let val3 = json!({
            "zshcs": {
                "experimental": {
                    "hover": "not a boolean"
                }
            }
        });
        assert!(!extract_experimental_hover(Some(&val3)));
    }

    #[test]
    fn test_extract_definition_none_and_null() {
        assert!(!extract_experimental_definition(None));
        let val_null = json!(null);
        assert!(!extract_experimental_definition(Some(&val_null)));
        let val_empty = json!({});
        assert!(!extract_experimental_definition(Some(&val_empty)));
    }

    #[test]
    fn test_extract_definition_nested_zshcs() {
        let val = json!({
            "zshcs": {
                "experimental": {
                    "definition": true
                }
            }
        });
        let config = Config::from_value(Some(&val));
        assert!(config.experimental_definition());
        assert!(extract_experimental_definition(Some(&val)));

        let val_false = json!({
            "zshcs": {
                "experimental": {
                    "definition": false
                }
            }
        });
        let config_false = Config::from_value(Some(&val_false));
        assert!(!config_false.experimental_definition());
        assert!(!extract_experimental_definition(Some(&val_false)));
    }

    #[test]
    fn test_extract_definition_direct_experimental() {
        let val = json!({
            "experimental": {
                "definition": true
            }
        });
        let config = Config::from_value(Some(&val));
        assert!(config.experimental_definition());
        assert!(extract_experimental_definition(Some(&val)));

        let val_false = json!({
            "experimental": {
                "definition": false
            }
        });
        let config_false = Config::from_value(Some(&val_false));
        assert!(!config_false.experimental_definition());
        assert!(!extract_experimental_definition(Some(&val_false)));
    }

    #[test]
    fn test_extract_definition_nested_settings_wrapper() {
        let val = json!({
            "settings": {
                "zshcs": {
                    "experimental": {
                        "definition": true
                    }
                }
            }
        });
        let config = Config::from_value(Some(&val));
        assert!(config.experimental_definition());
        assert!(extract_experimental_definition(Some(&val)));

        let val_false = json!({
            "settings": {
                "zshcs": {
                    "experimental": {
                        "definition": false
                    }
                }
            }
        });
        let config_false = Config::from_value(Some(&val_false));
        assert!(!config_false.experimental_definition());
        assert!(!extract_experimental_definition(Some(&val_false)));
    }

    #[test]
    fn test_extract_definition_invalid_types() {
        let val1 = json!("invalid string");
        assert!(!extract_experimental_definition(Some(&val1)));

        let val2 = json!(123);
        assert!(!extract_experimental_definition(Some(&val2)));

        let val3 = json!({
            "zshcs": {
                "experimental": {
                    "definition": "not a boolean"
                }
            }
        });
        assert!(!extract_experimental_definition(Some(&val3)));
    }
}
