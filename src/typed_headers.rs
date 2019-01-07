use http::{self, header::HeaderValue};
use std::fmt::Display;

pub use hyperx::header;
pub use hyperx::header::*;
pub use hyperx::{Error, Result};

/// An extension trait adding "typed" methods to `http::HeaderMap`.
pub trait HeaderMapExt {
    /// Inserts the typed `Header` into this `HeaderMap`.
    fn typed_insert<H: Header + Display>(&mut self, header: H);

    /// Tries to find the header by name, and then decode it into `H`.
    fn typed_get<H: Header>(&self) -> Option<H>;

    /// Tries to find the header by name, and then decode it into `H`.
    fn typed_try_get<H: Header>(&self) -> Result<Option<H>>;
}

impl HeaderMapExt for http::HeaderMap {
    fn typed_insert<H: Header + Display>(&mut self, header: H) {
        let name = H::header_name();
        let value = HeaderValue::from_str(&format!("{}", header)).unwrap();
        if self.contains_key(name) {
            self.append(name, value);
        } else {
            self.insert(name, value);
        }
    }

    fn typed_get<H: Header>(&self) -> Option<H> {
        HeaderMapExt::typed_try_get(self).unwrap_or(None)
    }

    fn typed_try_get<H: Header>(&self) -> Result<Option<H>> {
        let mut values = self.get_all(H::header_name()).iter();
        if values.size_hint() == (0, Some(0)) {
            Ok(None)
        } else {
            let mut raw: Raw = values.next().unwrap().as_bytes().into();
            for l in values {
                raw.push(l.as_bytes());
            }
            H::parse_header(&mut raw).map(Some)
        }
    }
}
