use std::borrow::Cow;
use std::io::BufWriter;
use std::io::{Read, Write};

use xml;
use xml::common::XmlVersion;
use xml::writer::EventWriter;
use xml::writer::XmlEvent as XmlWEvent;
use xml::EmitterConfig;

use xmltree::{self, Element};

use crate::{DavError, DavResult};

pub(crate) trait ElementExt {
    fn ns<S: Into<String>>(self, prefix: S, namespace: S) -> Self;
    fn new2<'a, E: Into<&'a str>>(e: E) -> Self;
    fn parse2<R: Read>(r: R) -> Result<Element, DavError>;
    fn new_text<'a, E: Into<&'a str>, T: Into<String>>(e: E, t: T) -> Self;
    fn text<'a, T: Into<String>>(self, t: T) -> Self;
    fn push(&mut self, e: Element);
    fn has_children(&self) -> bool;
    fn write_ev<W: Write>(&self, emitter: &mut EventWriter<W>) -> xml::writer::Result<()>;
}

impl ElementExt for Element {
    fn ns<S: Into<String>>(mut self, prefix: S, namespace: S) -> Element {
        let mut ns = self.namespaces.unwrap_or(xmltree::Namespace::empty());
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

    fn new_text<'a, N: Into<&'a str>, S: Into<String>>(n: N, t: S) -> Element {
        let mut e = Element::new2(n);
        e.text = Some(t.into());
        e
    }

    fn text<S: Into<String>>(mut self, t: S) -> Element {
        self.text = Some(t.into());
        self
    }

    fn push(&mut self, e: Element) {
        self.children.push(e);
    }

    fn has_children(&self) -> bool {
        !self.children.is_empty()
    }

    fn parse2<R: Read>(r: R) -> Result<Element, DavError> {
        match Element::parse(r) {
            Ok(elems) => Ok(elems),
            Err(xmltree::ParseError::MalformedXml(_)) => Err(DavError::XmlParseError),
            Err(_) => Err(DavError::XmlReadError),
        }
    }

    fn write_ev<W: Write>(&self, emitter: &mut EventWriter<W>) -> xml::writer::Result<()> {
        use xml::attribute::Attribute;
        use xml::name::Name;
        use xml::writer::events::XmlEvent;
        use xmltree::Namespace;

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
                name:  Name::local(k),
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
            name:       name,
            attributes: Cow::Owned(attributes),
            namespace:  unsafe { std::mem::transmute(namespace) }, // see xmltree-rs pull request #16
        })?;
        if let Some(ref t) = self.text {
            emitter.write(XmlEvent::Characters(t))?;
        }
        for elem in &self.children {
            elem.write_ev(emitter)?;
        }
        emitter.write(XmlEvent::EndElement { name: Some(name) })
    }
}

pub(crate) fn emitter<W: Write>(w: W) -> DavResult<EventWriter<BufWriter<W>>> {
    let mut emitter = EventWriter::new_with_config(
        BufWriter::new(w),
        EmitterConfig {
            perform_indent: false,
            indent_string: Cow::Borrowed(""),
            ..Default::default()
        },
    );
    emitter.write(XmlWEvent::StartDocument {
        version:    XmlVersion::Version10,
        encoding:   Some("utf-8"),
        standalone: None,
    })?;
    Ok(emitter)
}
