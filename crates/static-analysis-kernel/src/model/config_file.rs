use globset::{GlobBuilder, GlobMatcher};
use indexmap::IndexMap;
use serde;
use serde::{Deserialize, Serialize};
use std::borrow::Borrow;
use std::fmt;
use std::path::{Path, PathBuf};

use crate::config_file::{
    deserialize_category, deserialize_rule_configs, deserialize_ruleset_configs,
    deserialize_schema_version, get_default_schema_version, serialize_ruleset_configs,
};
use crate::model::rule::{RuleCategory, RuleSeverity};

// A pattern for an 'only' or 'ignore' field. The 'glob' field contains a precompiled glob pattern,
// while the 'prefix' field contains a path prefix.
#[derive(Deserialize, Serialize, Debug, Default, Clone)]
#[serde(from = "String", into = "String")]
pub struct PathPattern {
    pub glob: Option<GlobMatcher>,
    pub prefix: PathBuf,
}

// Lists of directories and glob patterns to include/exclude from the analysis.
#[derive(Deserialize, Serialize, Debug, PartialEq, Default, Clone)]
pub struct PathConfig {
    // Analyze only these directories and patterns.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub only: Option<Vec<PathPattern>>,
    // Do not analyze any of these directories and patterns.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ignore: Vec<PathPattern>,
}

#[derive(Debug, PartialEq, Default, Clone)]
pub struct ArgumentValues {
    pub by_subtree: IndexMap<String, String>,
}

// Configuration for a single rule.
#[derive(Deserialize, Serialize, Debug, PartialEq, Default, Clone)]
pub struct RuleConfig {
    // Paths to include/exclude for this rule.
    #[serde(flatten)]
    pub paths: PathConfig,
    #[serde(default, skip_serializing_if = "IndexMap::is_empty")]
    pub arguments: IndexMap<String, ArgumentValues>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub severity: Option<RuleSeverity>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_category",
        default
    )]
    pub category: Option<RuleCategory>,
}

// Configuration for a ruleset.
#[derive(Deserialize, Serialize, Debug, PartialEq, Default, Clone)]
pub struct RulesetConfig {
    // Paths to include/exclude for all rules in this ruleset.
    #[serde(flatten)]
    pub paths: PathConfig,
    // Rule-specific configurations.
    #[serde(
        default,
        deserialize_with = "deserialize_rule_configs",
        skip_serializing_if = "IndexMap::is_empty"
    )]
    pub rules: IndexMap<String, RuleConfig>,
}

// The parsed configuration file without any legacy fields.
#[derive(Deserialize, Serialize, Debug, PartialEq, Default)]
#[serde(from = "RawConfigFile")]
pub struct ConfigFile {
    #[serde(rename = "schema-version")]
    pub schema_version: String,
    // Configurations for the rulesets.
    #[serde(serialize_with = "serialize_ruleset_configs")]
    pub rulesets: IndexMap<String, RulesetConfig>,
    // Paths to include/exclude from analysis.
    #[serde(flatten)]
    pub paths: PathConfig,
    // Ignore all the paths in the .gitignore file.
    #[serde(rename = "ignore-gitignore", skip_serializing_if = "Option::is_none")]
    pub ignore_gitignore: Option<bool>,
    // Analyze only files up to this size.
    #[serde(rename = "max-file-size-kb", skip_serializing_if = "Option::is_none")]
    pub max_file_size_kb: Option<u64>,
    #[serde(
        rename = "ignore-generated-files",
        skip_serializing_if = "Option::is_none"
    )]
    pub ignore_generated_files: Option<bool>,
}

// The raw configuration file format with legacy fields and other quirks.
#[derive(Deserialize)]
struct RawConfigFile {
    // Version this configuration file complies with.
    #[serde(
        rename = "schema-version",
        default = "get_default_schema_version",
        deserialize_with = "deserialize_schema_version"
    )]
    schema_version: String,
    // Configurations for the rulesets.
    #[serde(deserialize_with = "deserialize_ruleset_configs")]
    rulesets: IndexMap<String, RulesetConfig>,
    // Paths to include/exclude from analysis.
    #[serde(flatten)]
    paths: PathConfig,
    // For backwards compatibility. Its content will be added to paths.ignore.
    #[serde(rename = "ignore-paths")]
    ignore_paths: Option<Vec<PathPattern>>,
    // Ignore all the paths in the .gitignore file.
    #[serde(rename = "ignore-gitignore")]
    ignore_gitignore: Option<bool>,
    // Analyze only files up to this size.
    #[serde(rename = "max-file-size-kb")]
    max_file_size_kb: Option<u64>,
    #[serde(rename = "ignore-generated-files")]
    ignore_generated_files: Option<bool>,
}

impl From<RawConfigFile> for ConfigFile {
    fn from(value: RawConfigFile) -> Self {
        ConfigFile {
            schema_version: value.schema_version,
            rulesets: value.rulesets,
            paths: {
                let mut paths = value.paths;
                if let Some(ignore) = value.ignore_paths {
                    paths.ignore.extend(ignore);
                }
                paths
            },
            ignore_gitignore: value.ignore_gitignore,
            max_file_size_kb: value.max_file_size_kb,
            ignore_generated_files: value.ignore_generated_files,
        }
    }
}

impl fmt::Display for ConfigFile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self)
    }
}

impl PathPattern {
    pub fn matches(&self, path: &str) -> bool {
        self.glob
            .as_ref()
            .map(|g| g.is_match(path))
            .unwrap_or(false)
            || Path::new(path).starts_with(&self.prefix)
    }
}

impl From<String> for PathPattern {
    fn from(value: String) -> Self {
        PathPattern {
            glob: GlobBuilder::new(&value)
                .literal_separator(true)
                .empty_alternates(true)
                .backslash_escape(true)
                .build()
                .map(|g| g.compile_matcher())
                .ok(),
            prefix: PathBuf::from(value),
        }
    }
}

impl Borrow<str> for PathPattern {
    fn borrow(&self) -> &str {
        self.prefix.to_str().unwrap_or("")
    }
}

impl From<PathPattern> for String {
    fn from(value: PathPattern) -> Self {
        value.prefix.display().to_string()
    }
}

impl PartialEq for PathPattern {
    fn eq(&self, other: &Self) -> bool {
        self.prefix.eq(&other.prefix)
    }
}
