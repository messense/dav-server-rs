//! Common filter types shared between CalDAV and CardDAV
//!
//! This module contains filter structures used by both CalDAV (RFC 4791)
//! and CardDAV (RFC 6352) REPORT requests.

/// Text matching filter for property values
///
/// Used in both CalDAV and CardDAV for matching text content in properties.
#[derive(Debug, Clone, Default)]
pub struct TextMatch {
    /// The text to match against
    pub text: String,
    /// Collation to use for comparison (e.g., "i;ascii-casemap")
    pub collation: Option<String>,
    /// If true, the match condition is negated
    pub negate_condition: bool,
    /// Match type for CardDAV: "equals", "contains", "starts-with", "ends-with"
    /// CalDAV uses "contains" by default if not specified
    pub match_type: Option<String>,
}

/// Parameter filter for matching property parameters
///
/// Used in both CalDAV and CardDAV for filtering based on property parameters
/// (e.g., TYPE=HOME on a TEL property).
#[derive(Debug, Clone)]
pub struct ParameterFilter {
    /// Name of the parameter to filter on (e.g., "TYPE")
    pub name: String,
    /// If true, the parameter must NOT be defined
    pub is_not_defined: bool,
    /// Text match filter for the parameter value
    pub text_match: Option<TextMatch>,
}

impl ParameterFilter {
    /// Create a new parameter filter with the given name
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            is_not_defined: false,
            text_match: None,
        }
    }
}
