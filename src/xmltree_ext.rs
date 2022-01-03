use std::borrow::Cow;
use std::io::{Read, Write};

use xml::common::XmlVersion;
use xml::writer::EventWriter;
use xml::writer::XmlEvent as XmlWEvent;
use xml::EmitterConfig;

use xmltree::{self, Element, XMLNode};

use crate::{DavError, DavResult};

pub(crate) trait ElementExt {
    /// Builder.
    fn new2<'a, E: Into<&'a str>>(e: E) -> Self;
    /// Builder.
    fn ns<S: Into<String>>(self, prefix: S, namespace: S) -> Self;
    /// Builder.
    fn text<T: Into<String>>(self, t: T) -> Self;
    /// Like parse, but returns DavError.
    fn parse2<R: Read>(r: R) -> Result<Element, DavError>;
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

    fn parse2<R: Read>(r: R) -> Result<Element, DavError> {
        let res = Element::parse(r);
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
