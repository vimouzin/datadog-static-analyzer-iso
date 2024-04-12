// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License, Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2024 Datadog, Inc.

use crate::check::Check;
use crate::rule_file::matcher::RawMatcher;
use crate::rule_file::validator::http::RawExtension;
use crate::rule_file::validator::RawValidator;
use crate::rule_file::{parse_candidate_variable, CandidateVariable, RawRuleFile};
use crate::validator::http;
use secrets_core::engine::{Engine, EngineBuilder, ValidationResult};
use secrets_core::matcher::hyperscan::HyperscanBuilder;
use secrets_core::matcher::{MatcherId, PatternId};
use secrets_core::rule::{RuleId, TargetedChecker};
use secrets_core::validator::http::RetryConfig;
use secrets_core::validator::{Candidate, ValidatorId};
use secrets_core::{Matcher, Rule, Validator};
use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::{fs, io};

#[derive(Debug, thiserror::Error)]
pub enum ScannerError {
    #[error("engine error: {message}")]
    Engine { message: String },
    #[error(transparent)]
    Io(#[from] io::Error),
}

pub struct Scanner {
    engine: Engine,
}

impl Scanner {
    pub fn scan_file(&self, file_path: &Path) -> Result<Vec<Candidate>, ScannerError> {
        let file_contents = fs::read(file_path).map_err(ScannerError::Io)?;
        self.engine
            .scan(file_path, &file_contents)
            .map_err(|err| ScannerError::Engine {
                message: err.to_string(),
            })
    }

    pub fn validate_candidate(
        &self,
        candidate: &Candidate,
    ) -> Result<ValidationResult, ScannerError> {
        self.engine
            .validate_candidate(candidate.clone())
            .map_err(|err| ScannerError::Engine {
                message: err.to_string(),
            })
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ScannerBuilderError {
    #[error("duplicate rule id: `{0}`")]
    DuplicateRuleId(String),
    #[error("error compiling rule `{rule}`: {message}")]
    RuleCompilationError { rule: String, message: String },
    #[error("{message}")]
    InvalidYamlSyntax { message: String },
    #[error("{0}")]
    CompilationError(String),
    #[error(transparent)]
    Io(#[from] io::Error),
}

#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub enum RuleSource {
    /// A file to be read
    YamlFile(PathBuf),
    /// A string that will be parsed as YAML.
    YamlLiteral(String),
}

/// A builder for a [`Scanner`].
#[derive(Default)]
pub struct ScannerBuilder {
    rule_sources: Vec<RuleSource>,
    /// `Rule` has a one-to-many relationship with `Pattern`
    rule_mapping: HashMap<RuleId, PatternId>,
    // ---
    // Validator-specific configuration
    http_retry: RetryConfig,
    // ---
    // Built items
    hs_builder: HyperscanBuilder,
    built_validators: Vec<Box<dyn Validator + Send + Sync>>,
    built_rules: Vec<Rule>,
}

impl ScannerBuilder {
    /// Instantiates a new builder for a [`Scanner`].
    pub fn new() -> Self {
        // (This ID is an arbitrary number, and is only hardcoded here because only one Matcher is currently used)
        let matcher_id = MatcherId(1);
        Self {
            rule_sources: Vec::new(),
            rule_mapping: HashMap::new(),
            http_retry: RetryConfig::default(),
            hs_builder: HyperscanBuilder::new(matcher_id),
            built_validators: Vec::new(),
            built_rules: Vec::new(),
            rule_infos: Vec::new(),
        }
    }

    /// Adds a file path to be read and parsed as a YAML-defined rule.
    pub fn yaml_file(mut self, file_path: impl Into<PathBuf>) -> Self {
        self.rule_sources
            .push(RuleSource::YamlFile(file_path.into()));
        self
    }

    /// Adds a string containing YAML-defined rule.
    pub fn yaml_string(mut self, yaml_str: impl Into<String>) -> Self {
        self.rule_sources
            .push(RuleSource::YamlLiteral(yaml_str.into()));
        self
    }

    /// Configures the global retry settings for all [`HttpValidator`](http::HttpValidator)
    pub fn http_retry(mut self, config: &RetryConfig) -> Self {
        self.http_retry = config.clone();
        self
    }

    pub fn try_build(mut self) -> Result<Scanner, ScannerBuilderError> {
        let rule_sources = std::mem::take(&mut self.rule_sources);
        for rule_source in rule_sources {
            self.compile_rule_mut(rule_source)?;
        }
        let hs = self
            .hs_builder
            .try_compile()
            .map_err(|err| ScannerBuilderError::CompilationError(err.to_string()))?;
        let engine = EngineBuilder::new()
            .matcher(Matcher::Hyperscan(hs))
            .validators(self.built_validators)
            .rules(self.built_rules)
            .build();
        Ok(Scanner { engine })
    }

    /// Compiles a rule, mutating all inner data as necessary.
    fn compile_rule_mut(&mut self, rule_source: RuleSource) -> Result<(), ScannerBuilderError> {
        let yaml_contents = match rule_source {
            RuleSource::YamlFile(path) => {
                fs::read_to_string(path).map_err(ScannerBuilderError::Io)?
            }
            RuleSource::YamlLiteral(literal) => literal,
        };
        let raw_rule = serde_yaml::from_str::<RawRuleFile>(&yaml_contents).map_err(|err| {
            ScannerBuilderError::InvalidYamlSyntax {
                message: err.to_string(),
            }
        })?;

        let rule_id: RuleId = raw_rule.id.into();
        let entry = match self.rule_mapping.entry(rule_id.clone()) {
            Entry::Occupied(_) => {
                return Err(ScannerBuilderError::DuplicateRuleId(rule_id.to_string()));
            }
            Entry::Vacant(entry) => entry,
        };

        let mut checks = Vec::new();
        let pattern_id = match raw_rule.matcher.deref() {
            RawMatcher::Hyperscan(raw) => {
                // Convert the user input into a Hyperscan pattern
                let pattern_id = self.hs_builder.add_regex(&raw.pattern).map_err(|err| {
                    ScannerBuilderError::RuleCompilationError {
                        rule: rule_id.to_string(),
                        message: err.to_string(),
                    }
                })?;
                entry.insert(pattern_id);

                // Convert the user input into a formatted `PatternCheck`
                if let Some(raw_checks) = &raw.checks {
                    for raw_check in raw_checks {
                        let check = Check::from_raw(raw_check);
                        let pattern_checker = match parse_candidate_variable(
                            raw_check.input_variable(),
                        ) {
                            None => {
                                return Err(ScannerBuilderError::RuleCompilationError { rule: rule_id.to_string(), message: format!("`{}` is not a valid variable: expecting either \"candidate\" or a capture name prepended by \"candidate.captures.\"", raw_check.input_variable()) });
                            }
                            Some(CandidateVariable::Entire) => TargetedChecker::candidate(check),
                            Some(CandidateVariable::Capture(name)) => {
                                TargetedChecker::named_capture(name, check)
                            }
                        };
                        checks.push(pattern_checker);
                    }
                }
                pattern_id
            }
        };

        let validator = match raw_rule.validator.deref() {
            RawValidator::Http(raw_http) => match &raw_http.0 {
                RawExtension::Simple(raw_cfg) => {
                    // Because it's derived from rule_id, this is a unique id.
                    let validator_id = ValidatorId::from(format!("validator-http_{}", rule_id));
                    http::build_simple_http(raw_cfg.clone(), validator_id, &self.http_retry)
                }
            },
        };

        let validator_id = validator.id().clone();
        let rule = Rule::new(rule_id, pattern_id, validator_id, Vec::new(), checks);
        self.built_rules.push(rule);

        let boxed: Box<dyn Validator + Send + Sync> = Box::new(validator);
        self.built_validators.push(boxed);

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::scanner::ScannerBuilder;
    use httpmock::MockServer;
    use std::path::PathBuf;

    const RULE_FILE: &str = "\
schema-version: v1
id: rule-one
matcher:
  hyperscan:
    pattern: (?<org_id>[a-z]{3})_[[:xdigit:]]{8}
    checks:
      - any-of:
          input: ${{ candidate.captures.org_id }}
          values: ['abc', 'xyz']
validator:
  http:
    extension: simple-request
    config:
      request:
        url: <__cfg(test)_magic_url__>/?id=${{ candidate.captures.org_id }}
        method: GET
        headers:
          Authorization: Bearer ${{ candidate }}
      response-handler:
        handler-list:
        default-result:
          secret: INCONCLUSIVE
          severity: NOTICE
";

    /// Tests the proper construction of `Scanner`, from correct Matcher to correct Validator to correct Rule
    #[test]
    fn matcher_captures_exported() {
        let ms = MockServer::start();
        let mock = ms.mock(|when, then| {
            when.method("GET")
                .path("/")
                .query_param("id", "abc")
                .header("Authorization", "Bearer abc_018cf028");
            then.status(200);
        });
        let yaml = RULE_FILE.replace("<__cfg(test)_magic_url__>", &ms.base_url());
        let scanner = ScannerBuilder::new().yaml_string(yaml).try_build().unwrap();

        let file_contents = "--- abc_018cf028 ---";
        let candidates = scanner
            .engine
            .scan(&PathBuf::new(), file_contents.as_bytes())
            .unwrap();
        // We only need to check that the HTTP request was sent with the captures substituted, not the result.
        let _ = scanner.engine.validate_candidate(candidates[0].clone());
        mock.assert_hits(1);
    }
}
