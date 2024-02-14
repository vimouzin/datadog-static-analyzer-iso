use crate::model::config_file::PathConfig;
use crate::path_restrictions::PathRestrictions;
use kernel::model::common::OutputFormat;
use kernel::model::rule::Rule;
use sha2::{Digest, Sha256};

/// represents the CLI configuratoin
#[derive(Clone)]
pub struct CliConfiguration {
    pub use_debug: bool,
    pub use_configuration_file: bool,
    pub ignore_gitignore: bool,
    pub source_directory: String,
    pub source_subdirectories: Vec<String>,
    pub path_config: PathConfig,
    pub rules_file: Option<String>,
    pub output_format: OutputFormat, // SARIF or JSON
    pub output_file: String,
    pub num_cpus: usize, // of cpus to use for parallelism
    pub rules: Vec<Rule>,
    pub path_restrictions: PathRestrictions,
    pub max_file_size_kb: u64,
    pub use_staging: bool,
}

impl CliConfiguration {
    /// Generate a digest to include in SARIF files to indicate what configuration and rules were used
    /// to run the analysis. To compute the digest, we take the attributes that are important to
    /// run and replicate the analysis such as the ignored paths and rules.
    pub fn generate_diff_aware_digest(&self) -> String {
        let rules_string: Vec<String> = self
            .rules
            .iter()
            .map(|r| r.get_config_hash_string())
            .collect();

        let full_config_string = format!(
            "{}:{}:{}:{}::{}:{}",
            self.path_config
                .ignore
                .as_ref()
                .map_or("".to_string(), |v| v.join(",")),
            self.path_config
                .only
                .as_ref()
                .map_or("".to_string(), |v| v.join(",")),
            self.ignore_gitignore,
            rules_string.join(","),
            self.max_file_size_kb,
            self.source_subdirectories.join(",")
        );
        // compute the hash using sha2
        format!("{:x}", Sha256::digest(full_config_string.as_bytes()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kernel::model::common::Language;
    use kernel::model::common::OutputFormat::Sarif;
    use kernel::model::rule::{RuleCategory, RuleSeverity, RuleType};
    use std::collections::HashMap;

    #[test]
    fn test_generate_diff_aware_hash() {
        let cli_configuration = CliConfiguration {
            use_debug: true,
            use_configuration_file: true,
            ignore_gitignore: true,
            source_directory: "bla".to_string(),
            source_subdirectories: vec![],
            path_config: PathConfig::default(),
            rules_file: None,
            output_format: Sarif, // SARIF or JSON
            output_file: "foo".to_string(),
            num_cpus: 2, // of cpus to use for parallelism
            rules: vec![Rule {
                name: "myrule".to_string(),
                short_description_base64: Some("bla".to_string()),
                description_base64: Some("bli".to_string()),
                category: RuleCategory::BestPractices,
                severity: RuleSeverity::Warning,
                language: Language::Python,
                rule_type: RuleType::TreeSitterQuery,
                entity_checked: None,
                code_base64: "mycode".to_string(),
                checksum: "foobar".to_string(),
                pattern: None,
                cwe: None,
                tree_sitter_query_base64: None,
                variables: HashMap::new(),
                tests: vec![],
            }],
            path_restrictions: PathRestrictions::default(),
            max_file_size_kb: 1,
            use_staging: false,
        };
        assert_eq!(
            cli_configuration.generate_diff_aware_digest(),
            "aadc07afa2ab7afb253e52a9be80bf7a756f953ce1f6de80f8717f0fa9584360"
        );
    }
}
