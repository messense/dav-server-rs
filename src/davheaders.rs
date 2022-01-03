use std::convert::TryFrom;
use std::fmt::Display;
use std::str::FromStr;

use headers::Header;
use http::header::{HeaderName, HeaderValue};
use lazy_static::lazy_static;
use regex::Regex;

use crate::fs::DavMetaData;

lazy_static! {
    static ref RE_URL: Regex = Regex::new(r"https?://[^/]*([^#?]+).*$").unwrap();
    pub static ref DEPTH: HeaderName = HeaderName::from_static("depth");
    pub static ref TIMEOUT: HeaderName = HeaderName::from_static("timeout");
    pub static ref OVERWRITE: HeaderName = HeaderName::from_static("overwrite");
    pub static ref DESTINATION: HeaderName = HeaderName::from_static("destination");
    pub static ref ETAG: HeaderName = HeaderName::from_static("etag");
    pub static ref IF_RANGE: HeaderName = HeaderName::from_static("if-range");
    pub static ref IF_MATCH: HeaderName = HeaderName::from_static("if-match");
    pub static ref IF_NONE_MATCH: HeaderName = HeaderName::from_static("if-none-match");
    pub static ref X_UPDATE_RANGE: HeaderName = HeaderName::from_static("x-update-range");
    pub static ref IF: HeaderName = HeaderName::from_static("if");
    pub static ref CONTENT_LANGUAGE: HeaderName = HeaderName::from_static("content-language");
}

// helper.
fn one<'i, I>(values: &mut I) -> Result<&'i HeaderValue, headers::Error>
where
    I: Iterator<Item = &'i HeaderValue>,
{
    let v = values.next().ok_or_else(invalid)?;
    if values.next().is_some() {
        Err(invalid())
    } else {
        Ok(v)
    }
}

// helper
fn invalid() -> headers::Error {
    headers::Error::invalid()
}

// helper
fn map_invalid(_e: impl std::error::Error) -> headers::Error {
    headers::Error::invalid()
}

macro_rules! header {
    ($tname:ident, $hname:ident, $sname:expr) => {
        lazy_static! {
            pub static ref $hname: HeaderName = HeaderName::from_static($sname);
        }

        #[derive(Debug, Clone, PartialEq)]
        pub struct $tname(pub String);

        impl Header for $tname {
            fn name() -> &'static HeaderName {
                &$hname
            }

            fn decode<'i, I>(values: &mut I) -> Result<Self, headers::Error>
            where
                I: Iterator<Item = &'i HeaderValue>,
            {
                one(values)?
                    .to_str()
                    .map(|x| $tname(x.to_owned()))
                    .map_err(map_invalid)
            }

            fn encode<E>(&self, values: &mut E)
            where
                E: Extend<HeaderValue>,
            {
                let value = HeaderValue::from_str(&self.0).unwrap();
                values.extend(std::iter::once(value))
            }
        }
    };
}

header!(ContentType, CONTENT_TYPE, "content-type");
header!(ContentLocation, CONTENT_LOCATION, "content-location");
header!(LockToken, LOCK_TOKEN, "lock-token");
header!(XLitmus, X_LITMUS, "x-litmus");

/// Depth: header.
#[derive(Debug, Copy, Clone, PartialEq)]
pub enum Depth {
    Zero,
    One,
    Infinity,
}

impl Header for Depth {
    fn name() -> &'static HeaderName {
        &DEPTH
    }

    fn decode<'i, I>(values: &mut I) -> Result<Self, headers::Error>
    where
        I: Iterator<Item = &'i HeaderValue>,
    {
        let value = one(values)?;
        match value.as_bytes() {
            b"0" => Ok(Depth::Zero),
            b"1" => Ok(Depth::One),
            b"infinity" | b"Infinity" => Ok(Depth::Infinity),
            _ => Err(invalid()),
        }
    }

    fn encode<E>(&self, values: &mut E)
    where
        E: Extend<HeaderValue>,
    {
        let value = match *self {
            Depth::Zero => "0",
            Depth::One => "1",
            Depth::Infinity => "Infinity",
        };
        values.extend(std::iter::once(HeaderValue::from_static(value)));
    }
}

/// Content-Language header.
#[derive(Debug, Clone, PartialEq)]
pub struct ContentLanguage(headers::Vary);

impl ContentLanguage {
    #[allow(dead_code)]
    pub fn iter_langs(&self) -> impl Iterator<Item = &str> {
        self.0.iter_strs()
    }
}

impl TryFrom<&str> for ContentLanguage {
    type Error = headers::Error;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        let value = HeaderValue::from_str(value).map_err(map_invalid)?;
        let mut values = std::iter::once(&value);
        ContentLanguage::decode(&mut values)
    }
}

impl Header for ContentLanguage {
    fn name() -> &'static HeaderName {
        &CONTENT_LANGUAGE
    }

    fn decode<'i, I>(values: &mut I) -> Result<Self, headers::Error>
    where
        I: Iterator<Item = &'i HeaderValue>,
    {
        let h = match headers::Vary::decode(values) {
            Err(e) => return Err(e),
            Ok(h) => h,
        };
        for lang in h.iter_strs() {
            let lang = lang.as_bytes();
            // **VERY** rudimentary check ...
            let ok = lang.len() == 2 || (lang.len() > 4 && lang[2] == b'-');
            if !ok {
                return Err(invalid());
            }
        }
        Ok(ContentLanguage(h))
    }

    fn encode<E>(&self, values: &mut E)
    where
        E: Extend<HeaderValue>,
    {
        self.0.encode(values)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum DavTimeout {
    Seconds(u32),
    Infinite,
}

#[derive(Debug, Clone)]
pub struct Timeout(pub Vec<DavTimeout>);

impl Header for Timeout {
    fn name() -> &'static HeaderName {
        &TIMEOUT
    }

    fn decode<'i, I>(values: &mut I) -> Result<Self, headers::Error>
    where
        I: Iterator<Item = &'i HeaderValue>,
    {
        let value = one(values)?;
        let mut v = Vec::new();
        let words = value.to_str().map_err(map_invalid)?.split(|c| c == ',');
        for word in words {
            let w = match word {
                "Infinite" => DavTimeout::Infinite,
                _ if word.starts_with("Second-") => {
                    let num = &word[7..];
                    match num.parse::<u32>() {
                        Err(_) => return Err(invalid()),
                        Ok(n) => DavTimeout::Seconds(n),
                    }
                }
                _ => return Err(invalid()),
            };
            v.push(w);
        }
        Ok(Timeout(v))
    }

    fn encode<E>(&self, values: &mut E)
    where
        E: Extend<HeaderValue>,
    {
        let mut first = false;
        let mut value = String::new();
        for s in &self.0 {
            if !first {
                value.push_str(", ");
            }
            first = false;
            match *s {
                DavTimeout::Seconds(n) => value.push_str(&format!("Second-{}", n)),
                DavTimeout::Infinite => value.push_str("Infinite"),
            }
        }
        values.extend(std::iter::once(HeaderValue::from_str(&value).unwrap()));
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Destination(pub String);

impl Header for Destination {
    fn name() -> &'static HeaderName {
        &DESTINATION
    }

    fn decode<'i, I>(values: &mut I) -> Result<Self, headers::Error>
    where
        I: Iterator<Item = &'i HeaderValue>,
    {
        let s = one(values)?.to_str().map_err(map_invalid)?;
        if s.starts_with('/') {
            return Ok(Destination(s.to_string()));
        }
        if let Some(caps) = RE_URL.captures(s) {
            if let Some(path) = caps.get(1) {
                return Ok(Destination(path.as_str().to_string()));
            }
        }
        Err(invalid())
    }

    fn encode<E>(&self, values: &mut E)
    where
        E: Extend<HeaderValue>,
    {
        values.extend(std::iter::once(HeaderValue::from_str(&self.0).unwrap()));
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Overwrite(pub bool);

impl Header for Overwrite {
    fn name() -> &'static HeaderName {
        &OVERWRITE
    }

    fn decode<'i, I>(values: &mut I) -> Result<Self, headers::Error>
    where
        I: Iterator<Item = &'i HeaderValue>,
    {
        let line = one(values)?;
        match line.as_bytes() {
            b"F" => Ok(Overwrite(false)),
            b"T" => Ok(Overwrite(true)),
            _ => Err(invalid()),
        }
    }

    fn encode<E>(&self, values: &mut E)
    where
        E: Extend<HeaderValue>,
    {
        let value = match self.0 {
            true => "T",
            false => "F",
        };
        values.extend(std::iter::once(HeaderValue::from_static(value)));
    }
}

#[derive(Debug, Clone)]
pub struct ETag {
    tag: String,
    weak: bool,
}

impl ETag {
    #[allow(dead_code)]
    pub fn new(weak: bool, t: impl Into<String>) -> Result<ETag, headers::Error> {
        let t = t.into();
        if t.contains('\"') {
            Err(invalid())
        } else {
            let w = if weak { "W/" } else { "" };
            Ok(ETag {
                tag: format!("{}\"{}\"", w, t),
                weak,
            })
        }
    }

    pub fn from_meta(meta: impl AsRef<dyn DavMetaData>) -> Option<ETag> {
        let tag = meta.as_ref().etag()?;
        Some(ETag {
            tag: format!("\"{}\"", tag),
            weak: false,
        })
    }

    #[allow(dead_code)]
    pub fn is_weak(&self) -> bool {
        self.weak
    }
}

impl FromStr for ETag {
    type Err = headers::Error;

    fn from_str(t: &str) -> Result<Self, Self::Err> {
        let (weak, s) = if let Some(t) = t.strip_prefix("W/") {
            (true, t)
        } else {
            (false, t)
        };
        if s.starts_with('\"') && s.ends_with('\"') && !s[1..s.len() - 1].contains('\"') {
            Ok(ETag {
                tag: t.to_owned(),
                weak,
            })
        } else {
            Err(invalid())
        }
    }
}

impl TryFrom<&HeaderValue> for ETag {
    type Error = headers::Error;

    fn try_from(value: &HeaderValue) -> Result<Self, Self::Error> {
        let s = value.to_str().map_err(map_invalid)?;
        ETag::from_str(s)
    }
}

impl Display for ETag {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "\"{}\"", self.tag)
    }
}

impl PartialEq for ETag {
    fn eq(&self, other: &Self) -> bool {
        !self.weak && !other.weak && self.tag == other.tag
    }
}

impl Header for ETag {
    fn name() -> &'static HeaderName {
        &ETAG
    }

    fn decode<'i, I>(values: &mut I) -> Result<Self, headers::Error>
    where
        I: Iterator<Item = &'i HeaderValue>,
    {
        let value = one(values)?;
        ETag::try_from(value)
    }

    fn encode<E>(&self, values: &mut E)
    where
        E: Extend<HeaderValue>,
    {
        values.extend(std::iter::once(HeaderValue::from_str(&self.tag).unwrap()));
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum IfRange {
    ETag(ETag),
    Date(headers::Date),
}

impl Header for IfRange {
    fn name() -> &'static HeaderName {
        &IF_RANGE
    }

    fn decode<'i, I>(values: &mut I) -> Result<Self, headers::Error>
    where
        I: Iterator<Item = &'i HeaderValue>,
    {
        let value = one(values)?;

        let mut iter = std::iter::once(value);
        if let Ok(tm) = headers::Date::decode(&mut iter) {
            return Ok(IfRange::Date(tm));
        }

        let mut iter = std::iter::once(value);
        if let Ok(et) = ETag::decode(&mut iter) {
            return Ok(IfRange::ETag(et));
        }

        Err(invalid())
    }

    fn encode<E>(&self, values: &mut E)
    where
        E: Extend<HeaderValue>,
    {
        match *self {
            IfRange::Date(ref d) => d.encode(values),
            IfRange::ETag(ref t) => t.encode(values),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ETagList {
    Tags(Vec<ETag>),
    Star,
}

#[derive(Debug, Clone, PartialEq)]
pub struct IfMatch(pub ETagList);

#[derive(Debug, Clone, PartialEq)]
pub struct IfNoneMatch(pub ETagList);

// Decode a list of etags. This is not entirely correct, we should
// actually use a real parser. E.g. we don't handle comma's in
// etags correctly - but we never generated those anyway.
fn decode_etaglist<'i, I>(values: &mut I) -> Result<ETagList, headers::Error>
where
    I: Iterator<Item = &'i HeaderValue>,
{
    let mut v = Vec::new();
    let mut count = 0usize;
    for value in values {
        let s = value.to_str().map_err(map_invalid)?;
        if s.trim() == "*" {
            return Ok(ETagList::Star);
        }
        for t in s.split(',') {
            // Simply skip misformed etags, they will never match.
            if let Ok(t) = ETag::from_str(t.trim()) {
                v.push(t);
            }
        }
        count += 1;
    }
    if count != 0 {
        Ok(ETagList::Tags(v))
    } else {
        Err(invalid())
    }
}

fn encode_etaglist<E>(m: &ETagList, values: &mut E)
where
    E: Extend<HeaderValue>,
{
    let value = match *m {
        ETagList::Star => "*".to_string(),
        ETagList::Tags(ref t) => t
            .iter()
            .map(|t| t.tag.as_str())
            .collect::<Vec<&str>>()
            .join(", "),
    };
    values.extend(std::iter::once(HeaderValue::from_str(&value).unwrap()));
}

impl Header for IfMatch {
    fn name() -> &'static HeaderName {
        &IF_MATCH
    }

    fn decode<'i, I>(values: &mut I) -> Result<Self, headers::Error>
    where
        I: Iterator<Item = &'i HeaderValue>,
    {
        Ok(IfMatch(decode_etaglist(values)?))
    }

    fn encode<E>(&self, values: &mut E)
    where
        E: Extend<HeaderValue>,
    {
        encode_etaglist(&self.0, values)
    }
}

impl Header for IfNoneMatch {
    fn name() -> &'static HeaderName {
        &IF_NONE_MATCH
    }

    fn decode<'i, I>(values: &mut I) -> Result<Self, headers::Error>
    where
        I: Iterator<Item = &'i HeaderValue>,
    {
        Ok(IfNoneMatch(decode_etaglist(values)?))
    }

    fn encode<E>(&self, values: &mut E)
    where
        E: Extend<HeaderValue>,
    {
        encode_etaglist(&self.0, values)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum XUpdateRange {
    FromTo(u64, u64),
    AllFrom(u64),
    Last(u64),
    Append,
}

impl Header for XUpdateRange {
    fn name() -> &'static HeaderName {
        &X_UPDATE_RANGE
    }

    fn decode<'i, I>(values: &mut I) -> Result<Self, headers::Error>
    where
        I: Iterator<Item = &'i HeaderValue>,
    {
        let mut s = one(values)?.to_str().map_err(map_invalid)?;
        if s == "append" {
            return Ok(XUpdateRange::Append);
        }
        if !s.starts_with("bytes=") {
            return Err(invalid());
        }
        s = &s[6..];

        let nums = s.split('-').collect::<Vec<&str>>();
        if nums.len() != 2 {
            return Err(invalid());
        }
        if !nums[0].is_empty() && !nums[1].is_empty() {
            return Ok(XUpdateRange::FromTo(
                (nums[0]).parse::<u64>().map_err(map_invalid)?,
                (nums[1]).parse::<u64>().map_err(map_invalid)?,
            ));
        }
        if !nums[0].is_empty() {
            return Ok(XUpdateRange::AllFrom(
                (nums[0]).parse::<u64>().map_err(map_invalid)?,
            ));
        }
        if !nums[1].is_empty() {
            return Ok(XUpdateRange::Last(
                (nums[1]).parse::<u64>().map_err(map_invalid)?,
            ));
        }
        Err(invalid())
    }

    fn encode<E>(&self, values: &mut E)
    where
        E: Extend<HeaderValue>,
    {
        let value = match *self {
            XUpdateRange::Append => "append".to_string(),
            XUpdateRange::FromTo(b, e) => format!("{}-{}", b, e),
            XUpdateRange::AllFrom(b) => format!("{}-", b),
            XUpdateRange::Last(e) => format!("-{}", e),
        };
        values.extend(std::iter::once(HeaderValue::from_str(&value).unwrap()));
    }
}

// The "If" header contains IfLists, of which the results are ORed.
#[derive(Debug, Clone, PartialEq)]
pub struct If(pub Vec<IfList>);

// An IfList contains Conditions, of which the results are ANDed.
#[derive(Debug, Clone, PartialEq)]
pub struct IfList {
    pub resource_tag: Option<url::Url>,
    pub conditions: Vec<IfCondition>,
}

// helpers.
impl IfList {
    fn new() -> IfList {
        IfList {
            resource_tag: None,
            conditions: Vec::new(),
        }
    }
    fn add(&mut self, not: bool, item: IfItem) {
        self.conditions.push(IfCondition { not, item });
    }
}

// Single Condition is [NOT] State-Token | ETag
#[derive(Debug, Clone, PartialEq)]
pub struct IfCondition {
    pub not: bool,
    pub item: IfItem,
}
#[derive(Debug, Clone, PartialEq)]
pub enum IfItem {
    StateToken(String),
    ETag(ETag),
}

// Below stuff is for the parser state.
#[derive(Debug, Clone, PartialEq)]
enum IfToken {
    ListOpen,
    ListClose,
    Not,
    Word(String),
    Pointy(String),
    ETag(ETag),
    End,
}

#[derive(Debug, Clone, PartialEq)]
enum IfState {
    Start,
    RTag,
    List,
    Not,
    Bad,
}

// helpers.
fn is_whitespace(c: u8) -> bool {
    b" \t\r\n".iter().any(|&x| x == c)
}
fn is_special(c: u8) -> bool {
    b"<>()[]".iter().any(|&x| x == c)
}

fn trim_left(mut out: &'_ [u8]) -> &'_ [u8] {
    while !out.is_empty() && is_whitespace(out[0]) {
        out = &out[1..];
    }
    out
}

// parse one token.
fn scan_until(buf: &[u8], c: u8) -> Result<(&[u8], &[u8]), headers::Error> {
    let mut i = 1;
    let mut quote = false;
    while quote || buf[i] != c {
        if buf.is_empty() || is_whitespace(buf[i]) {
            return Err(invalid());
        }
        if buf[i] == b'"' {
            quote = !quote;
        }
        i += 1
    }
    Ok((&buf[1..i], &buf[i + 1..]))
}

// scan one word.
fn scan_word(buf: &[u8]) -> Result<(&[u8], &[u8]), headers::Error> {
    for (i, &c) in buf.iter().enumerate() {
        if is_whitespace(c) || is_special(c) || c < 32 {
            if i == 0 {
                return Err(invalid());
            }
            return Ok((&buf[..i], &buf[i..]));
        }
    }
    Ok((buf, b""))
}

// get next token.
fn get_token(buf: &'_ [u8]) -> Result<(IfToken, &'_ [u8]), headers::Error> {
    let buf = trim_left(buf);
    if buf.is_empty() {
        return Ok((IfToken::End, buf));
    }
    match buf[0] {
        b'(' => Ok((IfToken::ListOpen, &buf[1..])),
        b')' => Ok((IfToken::ListClose, &buf[1..])),
        b'N' if buf.starts_with(b"Not") => Ok((IfToken::Not, &buf[3..])),
        b'<' => {
            let (tok, rest) = scan_until(buf, b'>')?;
            let s = std::string::String::from_utf8(tok.to_vec()).map_err(map_invalid)?;
            Ok((IfToken::Pointy(s), rest))
        }
        b'[' => {
            let (tok, rest) = scan_until(buf, b']')?;
            let s = std::str::from_utf8(tok).map_err(map_invalid)?;
            Ok((IfToken::ETag(ETag::from_str(s)?), rest))
        }
        _ => {
            let (tok, rest) = scan_word(buf)?;
            if tok == b"Not" {
                Ok((IfToken::Not, rest))
            } else {
                let s = std::string::String::from_utf8(tok.to_vec()).map_err(map_invalid)?;
                Ok((IfToken::Word(s), rest))
            }
        }
    }
}

impl Header for If {
    fn name() -> &'static HeaderName {
        &IF
    }

    fn decode<'i, I>(values: &mut I) -> Result<Self, headers::Error>
    where
        I: Iterator<Item = &'i HeaderValue>,
    {
        // one big state machine.
        let mut if_lists = If(Vec::new());
        let mut cur_list = IfList::new();

        let mut state = IfState::Start;
        let mut input = one(values)?.as_bytes();

        loop {
            let (tok, rest) = get_token(input)?;
            input = rest;
            state = match state {
                IfState::Start => match tok {
                    IfToken::ListOpen => IfState::List,
                    IfToken::Pointy(url) => {
                        let u = url::Url::parse(&url).map_err(map_invalid)?;
                        cur_list.resource_tag = Some(u);
                        IfState::RTag
                    }
                    IfToken::End => {
                        if !if_lists.0.is_empty() {
                            break;
                        }
                        IfState::Bad
                    }
                    _ => IfState::Bad,
                },
                IfState::RTag => match tok {
                    IfToken::ListOpen => IfState::List,
                    _ => IfState::Bad,
                },
                IfState::List | IfState::Not => {
                    let not = state == IfState::Not;
                    match tok {
                        IfToken::Not => {
                            if not {
                                IfState::Bad
                            } else {
                                IfState::Not
                            }
                        }
                        IfToken::Pointy(stok) | IfToken::Word(stok) => {
                            // as we don't have an URI parser, just
                            // check if there's at least one ':' in there.
                            if !stok.contains(':') {
                                IfState::Bad
                            } else {
                                cur_list.add(not, IfItem::StateToken(stok));
                                IfState::List
                            }
                        }
                        IfToken::ETag(etag) => {
                            cur_list.add(not, IfItem::ETag(etag));
                            IfState::List
                        }
                        IfToken::ListClose => {
                            if cur_list.conditions.is_empty() {
                                IfState::Bad
                            } else {
                                if_lists.0.push(cur_list);
                                cur_list = IfList::new();
                                IfState::Start
                            }
                        }
                        _ => IfState::Bad,
                    }
                }
                IfState::Bad => return Err(invalid()),
            };
        }
        Ok(if_lists)
    }

    fn encode<E>(&self, values: &mut E)
    where
        E: Extend<HeaderValue>,
    {
        let value = "[If header]";
        values.extend(std::iter::once(HeaderValue::from_static(value)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn if_header() {
        // Note that some implementations (golang net/x/webdav) also
        // accept a "plain  word" as StateToken, instead of only
        // a Coded-Url (<...>). We allow that as well, but I have
        // no idea if we need to (or should!).
        //let val = r#"  <http://x.yz/> ([W/"etag"] Not <DAV:nope> )
        //    (Not<urn:x>[W/"bla"] plain:word:123) "#;
        let val = r#"  <http://x.yz/> ([W/"etag"] Not <DAV:nope> ) (Not<urn:x>[W/"bla"] plain:word:123) "#;
        let hdrval = HeaderValue::from_static(val);
        let mut iter = std::iter::once(&hdrval);
        let hdr = If::decode(&mut iter);
        assert!(hdr.is_ok());
    }

    #[test]
    fn etag_header() {
        let t1 = ETag::from_str(r#"W/"12345""#).unwrap();
        let t2 = ETag::from_str(r#"W/"12345""#).unwrap();
        let t3 = ETag::from_str(r#""12346""#).unwrap();
        let t4 = ETag::from_str(r#""12346""#).unwrap();
        assert!(t1 != t2);
        assert!(t2 != t3);
        assert!(t3 == t4);
    }
}
