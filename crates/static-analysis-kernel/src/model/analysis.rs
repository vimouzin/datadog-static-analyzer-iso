use crate::model::common::Position;
use derive_builder::Builder;
use serde::{Deserialize, Serialize};

use std::collections::HashMap;

pub const ERROR_RULE_TIMEOUT: &str = "rule-timeout";
pub const ERROR_RULE_EXECUTION: &str = "error-execution";
pub const ERROR_INVALID_QUERY: &str = "error-invalid-query";

// Used internally to pass options to the analysis
#[derive(Clone, Deserialize, Debug, Serialize, Builder)]
pub struct AnalysisOptions {
    pub log_output: bool,
    pub use_debug: bool,
    pub ignore_generated_files: bool,
}

// Represent the lines to ignores for a file.
pub struct LinesToIgnore {
    pub lines_to_ignore_per_rule: HashMap<u32, Vec<String>>, // rules to ignore only for some files
    pub lines_to_ignore: Vec<u32>,                           // lines to ignore
    pub ignore_file: bool,                                   // if we should ignore all the file
    pub rules_to_ignore: Vec<String>,                        // rules to ignore in all the file
}

impl LinesToIgnore {
    /// return if a specific rule should be ignored
    ///  - rule_name is the full rule name like rule1/rule2
    ///  - line is the line of the violation
    /// returns true if the rule should be ignored
    pub fn should_filter_rule(&self, rule_name: &str, line: u32) -> bool {
        if self.ignore_file {
            return true;
        }

        let rule_name_string = rule_name.to_string();

        if self.rules_to_ignore.contains(&rule_name_string) {
            return true;
        }

        if self.lines_to_ignore.contains(&line) {
            return true;
        }

        if let Some(rules) = self.lines_to_ignore_per_rule.get(&line) {
            return rules.contains(&rule_name_string);
        }

        false
    }
}

// Used only internally
pub struct AnalysisContext {
    pub tree_sitter_tree: tree_sitter::Tree,
}

// Used for the node and this is externally visible.
// This is what you see when you do a .context on a node.
#[derive(Clone, Deserialize, Debug, Serialize, Builder)]
pub struct MatchNodeContext {
    pub code: Option<String>,
    pub filename: String,
    pub arguments: HashMap<String, String>,
}

// The node used to capture data in tree-sitter
#[derive(Clone, Deserialize, Debug, Serialize, Builder)]
pub struct TreeSitterNode {
    #[serde(rename = "astType")]
    pub ast_type: String,
    pub start: Position,
    pub end: Position,
    #[serde(rename = "fieldName")]
    pub field_name: Option<String>,
    pub children: Vec<TreeSitterNode>,
}

// The node that is then passed to the visit function.
#[derive(Clone, Debug, Serialize, Builder)]
pub struct MatchNode {
    pub captures: HashMap<String, TreeSitterNode>,
    #[serde(rename = "capturesList")]
    pub captures_list: HashMap<String, Vec<TreeSitterNode>>,
    pub context: MatchNodeContext,
}

#[cfg(test)]
mod tests {
    use crate::model::analysis::LinesToIgnore;
    use std::collections::HashMap;

    #[test]
    fn test_lines_to_ignore() {
        let mut lines_per_rule: HashMap<u32, Vec<String>> = HashMap::new();
        lines_per_rule.insert(13, vec!["ruleset/rule".to_string()]);

        let lines_to_ignore = LinesToIgnore {
            lines_to_ignore: vec![10, 42],
            lines_to_ignore_per_rule: lines_per_rule,
            ignore_file: false,
            rules_to_ignore: vec![],
        };

        assert!(!lines_to_ignore.should_filter_rule("foo/bar", 11));
        assert!(lines_to_ignore.should_filter_rule("foo/bar", 10));
        assert!(lines_to_ignore.should_filter_rule("ruleset/rule", 10));
        assert!(!lines_to_ignore.should_filter_rule("ruleset/rule", 11));
        assert!(lines_to_ignore.should_filter_rule("ruleset/rule", 13));
        assert!(!lines_to_ignore.should_filter_rule("foo/bar", 13));
    }

    // This should ignore everything
    #[test]
    fn test_lines_to_ignore_all_file() {
        let lines_to_ignore = LinesToIgnore {
            lines_to_ignore: vec![],
            lines_to_ignore_per_rule: HashMap::new(),
            ignore_file: true,
            rules_to_ignore: vec![],
        };

        assert!(lines_to_ignore.should_filter_rule("foo/bar", 11));
        assert!(lines_to_ignore.should_filter_rule("foo/bar", 10));
        assert!(lines_to_ignore.should_filter_rule("ruleset/rule", 10));
        assert!(lines_to_ignore.should_filter_rule("ruleset/rule", 11));
        assert!(lines_to_ignore.should_filter_rule("ruleset/rule", 13));
        assert!(lines_to_ignore.should_filter_rule("foo/bar", 13));
    }

    #[test]
    fn test_lines_to_ignore_one_rule_in_all_file() {
        let lines_to_ignore = LinesToIgnore {
            lines_to_ignore: vec![],
            lines_to_ignore_per_rule: HashMap::new(),
            ignore_file: false,
            rules_to_ignore: vec!["foo/bar".to_string()],
        };

        assert!(lines_to_ignore.should_filter_rule("foo/bar", 11));
        assert!(lines_to_ignore.should_filter_rule("foo/bar", 10));
        assert!(!lines_to_ignore.should_filter_rule("ruleset/rule", 10));
        assert!(!lines_to_ignore.should_filter_rule("ruleset/rule", 11));
        assert!(!lines_to_ignore.should_filter_rule("ruleset/rule", 13));
        assert!(lines_to_ignore.should_filter_rule("foo/bar", 13));
    }
}
