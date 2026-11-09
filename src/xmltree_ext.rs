use std::borrow::Cow;
use std::collections::HashMap;
use std::io::Write;
use xml::EmitterConfig;
use xml::common::XmlVersion;
use xml::reader::{ParserConfig, XmlEvent};
use xml::writer::EventWriter;
use xml::writer::XmlEvent as XmlWEvent;

use xmltree::{self, Element, XMLNode};

use crate::{DavError, DavResult};

// Bound recursive tree construction, serialization, cloning, and destruction.
// The document root counts as one level.
const MAX_XML_DEPTH: usize = 64;

pub(crate) trait ElementExt {
    /// Builder.
    fn new2<'a, E: Into<&'a str>>(e: E) -> Self;
    /// Builder.
    #[allow(dead_code)]
    fn new3(prefix: &str, name: &str, children: Vec<Element>) -> Self;
    /// Builder.
    fn ns<S: Into<String>>(self, prefix: S, namespace: S) -> Self;
    /// Builder.
    fn text<T: Into<String>>(self, t: T) -> Self;
    /// Like parse, but limits element depth to 64 and returns DavError.
    fn parse2(data: &[u8]) -> Result<Element, DavError>;
    /// Add a child element.
    fn push_element(&mut self, e: Element);
    /// Iterator over the children that are Elements.
    fn child_elems_into_iter(self) -> Box<dyn Iterator<Item = Element>>;
    /// Iterator over the children that are Elements.
    fn child_elems_iter<'a>(&'a self) -> Box<dyn Iterator<Item = &'a Element> + 'a>;
    /// Vec of the children that are Elements.
    fn take_child_elems(self) -> Vec<Element>;
    /// Does the element have children that are also Elements.
    fn has_child_elems(&self) -> bool;
    /// Write the element using an EventWriter.
    fn write_ev<W: Write>(&self, emitter: &mut EventWriter<W>) -> xml::writer::Result<()>;
}

impl ElementExt for Element {
    fn ns<S: Into<String>>(mut self, prefix: S, namespace: S) -> Element {
        let mut ns = self.namespaces.unwrap_or_else(xmltree::Namespace::empty);
        ns.force_put(prefix.into(), namespace.into());
        self.namespaces = Some(ns);
        self
    }

    fn new2<'a, N: Into<&'a str>>(n: N) -> Element {
        let v: Vec<&str> = n.into().splitn(2, ':').collect();
        if v.len() == 1 {
            Element::new(v[0])
        } else {
            let mut e = Element::new(v[1]);
            e.prefix = Some(v[0].to_string());
            e
        }
    }

    fn new3(prefix: &str, name: &str, children: Vec<Element>) -> Element {
        Element {
            prefix: Some(prefix.to_string()),
            namespace: None,
            namespaces: None,
            name: name.to_string(),
            attributes: HashMap::new(),
            children: children.into_iter().map(XMLNode::Element).collect(),
        }
    }

    fn text<S: Into<String>>(mut self, t: S) -> Element {
        let nodes = self
            .children
            .drain(..)
            .filter(|n| n.as_text().is_none())
            .collect();
        self.children = nodes;
        self.children.push(XMLNode::Text(t.into()));
        self
    }

    fn push_element(&mut self, e: Element) {
        self.children.push(XMLNode::Element(e));
    }

    fn child_elems_into_iter(self) -> Box<dyn Iterator<Item = Element>> {
        let iter = self.children.into_iter().filter_map(|n| match n {
            XMLNode::Element(e) => Some(e),
            _ => None,
        });
        Box::new(iter)
    }

    fn child_elems_iter<'a>(&'a self) -> Box<dyn Iterator<Item = &'a Element> + 'a> {
        let iter = self.children.iter().filter_map(|n| n.as_element());
        Box::new(iter)
    }

    fn take_child_elems(self) -> Vec<Element> {
        self.children
            .into_iter()
            .filter_map(|n| match n {
                XMLNode::Element(e) => Some(e),
                _ => None,
            })
            .collect()
    }

    fn has_child_elems(&self) -> bool {
        self.children.iter().find_map(|n| n.as_element()).is_some()
    }

    fn parse2(data: &[u8]) -> Result<Element, DavError> {
        // Match xmltree's parser configuration in both passes. Check depth with
        // the streaming reader before xmltree can recurse, even on invalid XML.
        let config = ParserConfig::new().ignore_comments(false);
        let mut depth = 0usize;
        for event in config.clone().create_reader(data) {
            match event.map_err(|_| DavError::XmlParseError)? {
                XmlEvent::StartElement { .. } => {
                    depth += 1;
                    if depth > MAX_XML_DEPTH {
                        return Err(DavError::XmlParseError);
                    }
                }
                XmlEvent::EndElement { .. } => depth -= 1,
                XmlEvent::EndDocument => break,
                _ => {}
            }
        }

        let res = Element::parse_with_config(data, config);
        match res {
            Ok(elems) => Ok(elems),
            Err(xmltree::ParseError::MalformedXml(_)) => Err(DavError::XmlParseError),
            Err(_) => Err(DavError::XmlReadError),
        }
    }

    fn write_ev<W: Write>(&self, emitter: &mut EventWriter<W>) -> xml::writer::Result<()> {
        use xml::attribute::Attribute;
        use xml::name::Name;
        use xml::namespace::Namespace;
        use xml::writer::events::XmlEvent;

        let mut name = Name::local(&self.name);
        if let Some(ref ns) = self.namespace {
            name.namespace = Some(ns);
        }
        if let Some(ref p) = self.prefix {
            name.prefix = Some(p);
        }

        let mut attributes = Vec::with_capacity(self.attributes.len());
        for (k, v) in &self.attributes {
            attributes.push(Attribute {
                name: Name::local(k),
                value: v,
            });
        }

        let empty_ns = Namespace::empty();
        let namespace = if let Some(ref ns) = self.namespaces {
            Cow::Borrowed(ns)
        } else {
            Cow::Borrowed(&empty_ns)
        };

        emitter.write(XmlEvent::StartElement {
            name,
            attributes: Cow::Owned(attributes),
            namespace,
        })?;
        for node in &self.children {
            match node {
                XMLNode::Element(elem) => elem.write_ev(emitter)?,
                XMLNode::Text(text) => emitter.write(XmlEvent::Characters(text))?,
                XMLNode::Comment(comment) => emitter.write(XmlEvent::Comment(comment))?,
                XMLNode::CData(comment) => emitter.write(XmlEvent::CData(comment))?,
                XMLNode::ProcessingInstruction(name, data) => match data.to_owned() {
                    Some(string) => emitter.write(XmlEvent::ProcessingInstruction {
                        name,
                        data: Some(&string),
                    })?,
                    None => emitter.write(XmlEvent::ProcessingInstruction { name, data: None })?,
                },
            }
            // elem.write_ev(emitter)?;
        }
        emitter.write(XmlEvent::EndElement { name: Some(name) })?;

        Ok(())
    }
}

pub(crate) fn emitter<W: Write>(w: W) -> DavResult<EventWriter<W>> {
    let mut emitter = EventWriter::new_with_config(
        w,
        EmitterConfig {
            perform_indent: false,
            indent_string: Cow::Borrowed(""),
            ..Default::default()
        },
    );
    emitter.write(XmlWEvent::StartDocument {
        version: XmlVersion::Version10,
        encoding: Some("utf-8"),
        standalone: None,
    })?;
    Ok(emitter)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nested_xml(depth: usize) -> String {
        "<a>".repeat(depth) + &"</a>".repeat(depth)
    }

    #[test]
    fn accepts_xml_at_depth_limit() {
        let xml = nested_xml(MAX_XML_DEPTH);
        let tree = Element::parse2(xml.as_bytes()).unwrap();
        // Also exercise recursive serialization and destruction at the limit.
        tree.write_ev(&mut emitter(Vec::new()).unwrap()).unwrap();
    }

    #[test]
    fn rejects_xml_above_depth_limit() {
        for depth in [MAX_XML_DEPTH + 1, 3_000] {
            let xml = nested_xml(depth);
            assert!(matches!(
                Element::parse2(xml.as_bytes()),
                Err(DavError::XmlParseError)
            ));
            let unclosed = "<a>".repeat(depth);
            assert!(matches!(
                Element::parse2(unclosed.as_bytes()),
                Err(DavError::XmlParseError)
            ));
        }
    }

    #[test]
    fn counts_nesting_rather_than_siblings_or_text() {
        let xml = format!(
            "<root><!-- {} --><![CDATA[{}]]>{}</root>",
            "<a>".repeat(100),
            "<a>".repeat(100),
            "<a/>".repeat(100),
        );
        let tree = Element::parse2(xml.as_bytes()).unwrap();
        assert!(matches!(tree.children[0], XMLNode::Comment(_)));
        assert!(matches!(tree.children[1], XMLNode::CData(_)));
        assert_eq!(tree.child_elems_iter().count(), 100);
    }

    #[test]
    fn enforces_depth_limit_for_utf16() {
        for depth in [MAX_XML_DEPTH, MAX_XML_DEPTH + 1] {
            let xml = format!(
                "<?xml version=\"1.0\" encoding=\"UTF-16\"?>{}",
                nested_xml(depth)
            );
            let data: Vec<u8> = [0xff, 0xfe]
                .into_iter()
                .chain(xml.encode_utf16().flat_map(u16::to_le_bytes))
                .collect();
            let result = Element::parse2(&data);
            if depth == MAX_XML_DEPTH {
                assert!(result.is_ok());
            } else {
                assert!(matches!(result, Err(DavError::XmlParseError)));
            }
        }
    }

    #[test]
    fn rejects_malformed_xml() {
        for xml in ["", "<a>", "<a></b>", "<a/><b>", "</a>"] {
            assert!(matches!(
                Element::parse2(xml.as_bytes()),
                Err(DavError::XmlParseError)
            ));
        }
    }
}
