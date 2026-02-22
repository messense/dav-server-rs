//! CardDAV (vCard Extensions to WebDAV) support
//!
//! This module provides CardDAV functionality on top of the base WebDAV implementation.
//! CardDAV is defined in RFC 6352 and provides standardized access to address book data
//! using the vCard format.

#[cfg(feature = "carddav")]
use calcard::vcard::VCard;
use xmltree::Element;

use crate::davpath::DavPath;

// Re-export shared filter types
pub use crate::dav_filters::{ParameterFilter, TextMatch};

// CardDAV XML namespaces
pub const NS_CARDDAV_URI: &str = "urn:ietf:params:xml:ns:carddav";

/// The default carddav directory, which is being used for the preprovided filesystems. Path is without trailing slash
pub const DEFAULT_CARDDAV_NAME: &str = "addressbooks";
pub const DEFAULT_CARDDAV_DIRECTORY: &str = "/addressbooks";
pub const DEFAULT_CARDDAV_DIRECTORY_ENDSLASH: &str = "/addressbooks/";

/// Default maximum resource size for address book entries (1MB)
pub const DEFAULT_MAX_RESOURCE_SIZE: u64 = 1024 * 1024;

/// CardDAV address book collection properties
#[derive(Debug, Clone)]
pub struct AddressBookProperties {
    pub description: Option<String>,
    pub max_resource_size: Option<u64>,
    pub display_name: Option<String>,
}

impl Default for AddressBookProperties {
    fn default() -> Self {
        Self {
            description: None,
            max_resource_size: Some(DEFAULT_MAX_RESOURCE_SIZE),
            display_name: None,
        }
    }
}

/// Address book query filters for REPORT requests
#[derive(Debug, Clone)]
pub struct AddressBookQuery {
    pub prop_filter: Option<PropertyFilter>,
    pub properties: Vec<String>,
    pub limit: Option<u32>,
}

/// CardDAV property filter
///
/// Note: CardDAV property filters are similar to CalDAV but without time_range.
/// Both use the shared TextMatch and ParameterFilter types.
#[derive(Debug, Clone)]
pub struct PropertyFilter {
    pub name: String,
    pub is_not_defined: bool,
    pub text_match: Option<TextMatch>,
    pub param_filters: Vec<ParameterFilter>,
}

/// CardDAV REPORT request types
#[derive(Debug, Clone)]
pub enum CardDavReportType {
    AddressBookQuery(AddressBookQuery),
    AddressBookMultiget { hrefs: Vec<String> },
}

/// Helper functions for CardDAV XML generation
pub fn create_supported_address_data() -> Element {
    let mut elem = Element::new("CARD:supported-address-data");
    elem.namespace = Some(NS_CARDDAV_URI.to_string());

    let mut address_data = Element::new("CARD:address-data-type");
    address_data.namespace = Some(NS_CARDDAV_URI.to_string());
    address_data
        .attributes
        .insert("content-type".to_string(), "text/vcard".to_string());
    address_data
        .attributes
        .insert("version".to_string(), "3.0".to_string());

    elem.children.push(xmltree::XMLNode::Element(address_data));

    // Also support vCard 4.0
    let mut address_data_v4 = Element::new("CARD:address-data-type");
    address_data_v4.namespace = Some(NS_CARDDAV_URI.to_string());
    address_data_v4
        .attributes
        .insert("content-type".to_string(), "text/vcard".to_string());
    address_data_v4
        .attributes
        .insert("version".to_string(), "4.0".to_string());

    elem.children
        .push(xmltree::XMLNode::Element(address_data_v4));

    elem
}

pub fn create_addressbook_home_set(prefix: &str, path: &str) -> Element {
    let mut elem = Element::new("CARD:addressbook-home-set");
    elem.namespace = Some(NS_CARDDAV_URI.to_string());

    let mut href = Element::new("D:href");
    href.namespace = Some("DAV:".to_string());
    href.children
        .push(xmltree::XMLNode::Text(format!("{prefix}{path}")));

    elem.children.push(xmltree::XMLNode::Element(href));
    elem
}

/// Check if a path is within the default CardDAV directory. Expects path without prefix.
pub(crate) fn is_path_in_carddav_directory(dav_path: &DavPath) -> bool {
    let path_string = dav_path.to_string();
    path_string.len() > DEFAULT_CARDDAV_DIRECTORY_ENDSLASH.len()
        && path_string.starts_with(DEFAULT_CARDDAV_DIRECTORY_ENDSLASH)
}

/// Check if content appears to be vCard data
pub fn is_vcard_data(content: &[u8]) -> bool {
    if !content.starts_with(b"BEGIN:VCARD") {
        return false;
    }

    let trimmed = content.trim_ascii_end();
    trimmed.ends_with(b"END:VCARD")
}

/// Validate vCard data using the calcard crate
///
/// This function validates that the content is a well-formed vCard.
/// Use this function in your application layer to validate vCard data
/// before or after writing to the filesystem.
///
/// # Example
///
/// ```ignore
/// use dav_server::carddav::validate_vcard_data;
///
/// let vcard = "BEGIN:VCARD\nVERSION:3.0\nFN:Test\nEND:VCARD";
/// match validate_vcard_data(vcard) {
///     Ok(_) => println!("Valid vCard"),
///     Err(e) => println!("Invalid vCard: {}", e),
/// }
/// ```
#[cfg(feature = "carddav")]
pub fn validate_vcard_data(content: &str) -> Result<VCard, String> {
    VCard::parse(content).map_err(|e| format!("Invalid vCard data: {:?}", e))
}

/// Validate vCard data and check for required properties
///
/// This is a stricter validation that ensures the vCard has required properties
/// like VERSION and FN (formatted name) as required by RFC 6350.
///
/// Returns an error message describing what's missing or invalid.
#[cfg(feature = "carddav")]
pub fn validate_vcard_strict(content: &str) -> Result<(), String> {
    // First, try to parse the vCard
    let vcard = validate_vcard_data(content)?;

    // Check for VERSION property
    if vcard.version().is_none() {
        return Err("Missing required VERSION property".to_string());
    }

    // FN is required in vCard 3.0 and 4.0
    // Check if FN exists in the parsed vCard or via string extraction
    if extract_vcard_fn(content).is_none() {
        return Err("Missing required FN (formatted name) property".to_string());
    }

    Ok(())
}

/// Extract the UID from vCard data
///
/// Handles both standard `UID:value` and grouped properties like `item1.UID:value`.
/// Also handles properties with parameters like `UID;VALUE=TEXT:value`.
pub fn extract_vcard_uid(content: &str) -> Option<String> {
    for line in content.lines() {
        let line = line.trim();
        if let Some(uid) = extract_vcard_property_value(line, "UID") {
            return Some(uid);
        }
    }
    None
}

/// Extract the FN (formatted name) from vCard data
///
/// Handles both standard `FN:value` and grouped properties like `item1.FN:value`.
/// Also handles properties with parameters like `FN;CHARSET=UTF-8:value`.
pub fn extract_vcard_fn(content: &str) -> Option<String> {
    for line in content.lines() {
        let line = line.trim();
        if let Some(fn_value) = extract_vcard_property_value(line, "FN") {
            return Some(fn_value);
        }
    }
    None
}

/// Helper function to extract a vCard property value, handling groups and parameters
///
/// Supports formats:
/// - `PROPERTY:value`
/// - `group.PROPERTY:value`
/// - `PROPERTY;param=val:value`
/// - `group.PROPERTY;param=val:value`
fn extract_vcard_property_value(line: &str, property_name: &str) -> Option<String> {
    // Find the colon that separates the property name from the value
    let colon_pos = line.find(':')?;
    let property_part = &line[..colon_pos];
    let value = &line[colon_pos + 1..];

    // Check if property part matches (with optional group prefix and parameters)
    // The property name is before any semicolon (which starts parameters)
    let name_part = property_part.split(';').next()?;

    // Check for group prefix (e.g., "item1.UID" -> "UID")
    let actual_name = if let Some(dot_pos) = name_part.find('.') {
        &name_part[dot_pos + 1..]
    } else {
        name_part
    };

    if actual_name.eq_ignore_ascii_case(property_name) {
        Some(value.to_string())
    } else {
        None
    }
}
