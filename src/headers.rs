
use std::fmt::{self, Write};
use std::str::FromStr;

use regex::Regex;
use url;

use crate::typed_headers::{self, EntityTag, Header, RawLike};

header! { (WWWAuthenticate, "WWW-Authenticate") => [String] }
header! { (DAV, "DAV") => [String] }
header! { (MSAuthorVia, "MS-Author-Via") => [String] }
header! { (ContentType, "Content-Type") => [String] }
header! { (LockToken, "Lock-Token") => [String] }
header! { (XLitmus, "X-Litmus") => [String] }
header! { (ContentLocation, "Content-Location") => [String] }

lazy_static! {
   static ref RE_URL : Regex = Regex::new(r"https?://[^/]*([^#?]+).*$").unwrap();
}

#[derive(Debug,Copy,Clone,PartialEq)]
pub enum Depth {
    Zero,
    One,
    Infinity,
}

impl Header for Depth {
    fn header_name() -> &'static str {
        "Depth"
    }

    fn parse_header<'a, T: RawLike<'a>>(raw: &'a T) -> typed_headers::Result<Depth> {
        if let Some(line) = raw.one() {
            match line {
                b"0" => return Ok(Depth::Zero),
                b"1" => return Ok(Depth::One),
                b"infinity" | b"Infinity" => return Ok(Depth::Infinity),
                _ => {},
            }
        }
        Err(typed_headers::Error::Header)
    }

    fn fmt_header(&self, f: &mut typed_headers::Formatter) -> fmt::Result {
        let value = match *self {
            Depth::Zero => "0",
            Depth::One => "1",
            Depth::Infinity => "Infinity",
        };
        f.fmt_line(&value)
    }
}

#[derive(Debug,Clone,PartialEq)]
pub enum DavTimeout {
    Seconds(u32),
    Infinite,
}

#[derive(Debug,Clone)]
pub struct Timeout(pub Vec<DavTimeout>);

impl Header for Timeout {
    fn header_name() -> &'static str {
        "Timeout"
    }

    fn parse_header<'a, T: RawLike<'a>>(raw: &'a T) -> typed_headers::Result<Timeout> {
        if let Some(line) = raw.one() {
            let mut v = Vec::new();
            let words = std::str::from_utf8(line)?.split(|c| c == ',');
            for word in words {
                let w = match word {
                    "Infinite" => DavTimeout::Infinite,
                    _ if word.starts_with("Second-") => {
                        let num = &word[7..];
                        match num.parse::<u32>() {
                            Err(_) => return Err(typed_headers::Error::Header),
                            Ok(n) => DavTimeout::Seconds(n),
                        }
                    },
                    _ => return Err(typed_headers::Error::Header),
                };
                v.push(w);
            }
            return Ok(Timeout(v));
        }
        Err(typed_headers::Error::Header)
    }

    fn fmt_header(&self, f: &mut typed_headers::Formatter) -> fmt::Result {
        let mut first = false;
        let mut value = String::new();
        for s in &self.0 {
            if !first {
                value.push_str(", ");
            }
            first = false;
            match s {
                &DavTimeout::Seconds(n) => write!(&mut value, "Second-{}", n)?,
                &DavTimeout::Infinite => value.push_str("Infinite"),
            }
        }
        f.fmt_line(&value)
    }
}

#[derive(Debug,Clone,PartialEq)]
pub struct Destination(pub String);

impl Header for Destination {
    fn header_name() -> &'static str {
        "Destination"
    }

    fn parse_header<'a, T: RawLike<'a>>(raw: &'a T) -> typed_headers::Result<Destination> {
        if let Some(line) = raw.one() {
            let s = match std::str::from_utf8(line) {
                Ok(s) => s,
                Err(_) => return Err(typed_headers::Error::Header),
            };
            if s.starts_with("/") {
                return Ok(Destination(s.to_string()));
            }
            match RE_URL.captures(s) {
                Some(caps) => {
                    if let Some(path) = caps.get(1) {
                        return Ok(Destination(path.as_str().to_string()));
                    }
                }
                _ => {},
            }
        }
        Err(typed_headers::Error::Header)
    }

    fn fmt_header(&self, f: &mut typed_headers::Formatter) -> fmt::Result {
        f.fmt_line(&self.0)
    }
}

#[derive(Debug,Clone,PartialEq)]
pub struct Overwrite(pub bool);

impl Header for Overwrite {
    fn header_name() -> &'static str {
        "Overwrite"
    }

    fn parse_header<'a, T: RawLike<'a>>(raw: &'a T) -> typed_headers::Result<Overwrite> {
        if let Some(line) = raw.one() {
            match line {
                b"F" => return Ok(Overwrite(false)),
                b"T" => return Ok(Overwrite(true)),
                _ => {},
            }
        }
        Err(typed_headers::Error::Header)
    }

    fn fmt_header(&self, f: &mut typed_headers::Formatter) -> fmt::Result {
        let value = match self.0 {
            true => "T",
            false => "F",
        };
        f.fmt_line(&value)
    }
}

#[derive(Debug,Clone,PartialEq)]
pub enum IfRange {
    EntityTag(typed_headers::EntityTag),
    Date(typed_headers::HttpDate),
}

impl Header for IfRange {
    fn header_name() -> &'static str {
        "If-Range"
    }

    fn parse_header<'a, T: RawLike<'a>>(raw: &'a T) -> typed_headers::Result<IfRange> {
        if let Some(line) = raw.one() {
            let s = match std::str::from_utf8(line) {
                Ok(s) => s,
                Err(e) => Err(e)?,
            };
            if let Ok(tm) = typed_headers::HttpDate::from_str(s) {
                return Ok(IfRange::Date(tm));
            }
            if let Ok(et) = typed_headers::EntityTag::from_str(s) {
                return Ok(IfRange::EntityTag(et));
            }
        }
        Err(typed_headers::Error::Header)
    }

    fn fmt_header(&self, f: &mut typed_headers::Formatter) -> fmt::Result {
        let value = match self {
            &IfRange::Date(ref d) => format!("{}", d),
            &IfRange::EntityTag(ref t) => t.tag().to_string(),
        };
        f.fmt_line(&value)
    }
}

#[derive(Debug,Clone,PartialEq)]
pub enum ETagList {
    Tags(Vec<typed_headers::EntityTag>),
    Star,
}

#[derive(Debug,Clone,PartialEq)]
pub struct IfMatch(pub ETagList);

#[derive(Debug,Clone,PartialEq)]
pub struct IfNoneMatch(pub ETagList);

fn parse_etag_list<'a, T: RawLike<'a>>(raw: &'a T) -> typed_headers::Result<ETagList> {
    if let Some(line) = raw.one() {
        let s = std::str::from_utf8(line)?;
        if s.trim() == "*" {
            return Ok(ETagList::Star);
        }
        let mut v = Vec::new();
        for t in s.split(',') {
            if let Ok(t) = typed_headers::EntityTag::from_str(t.trim()) {
                v.push(t);
            }
        }
        return Ok(ETagList::Tags(v));
    }
    Err(typed_headers::Error::Header)?
}

fn fmt_etaglist(m: &ETagList, f: &mut typed_headers::Formatter) -> fmt::Result {
    let value = match m {
        &ETagList::Star => "*".to_string(),
        &ETagList::Tags(ref t) =>
                t.iter().map(|t| t.tag()).collect::<Vec<&str>>().join(", ")
    };
    f.fmt_line(&value)
}

impl Header for IfMatch {
    fn header_name() -> &'static str {
        "If-Match"
    }

    fn parse_header<'a, T: RawLike<'a>>(raw: &'a T) -> typed_headers::Result<IfMatch> {
        Ok(IfMatch(parse_etag_list(raw)?))
    }

    fn fmt_header(&self, f: &mut typed_headers::Formatter) -> fmt::Result {
        fmt_etaglist(&self.0, f)
    }
}

impl Header for IfNoneMatch {
    fn header_name() -> &'static str {
        "If-None-Match"
    }

    fn parse_header<'a, T: RawLike<'a>>(raw: &'a T) -> typed_headers::Result<IfNoneMatch> {
        Ok(IfNoneMatch(parse_etag_list(raw)?))
    }

    fn fmt_header(&self, f: &mut typed_headers::Formatter) -> fmt::Result {
        fmt_etaglist(&self.0, f)
    }
}

#[derive(Debug,Clone,PartialEq)]
pub enum XUpdateRange {
    FromTo(u64, u64),
    AllFrom(u64),
    Last(u64),
    Append,
}

impl Header for XUpdateRange {
    fn header_name() -> &'static str {
        "X-Update-Range"
    }

    fn parse_header<'a, T: RawLike<'a>>(raw: &'a T) -> typed_headers::Result<XUpdateRange> {
        let line = raw.one().ok_or(typed_headers::Error::Header)?;
        let mut s = std::str::from_utf8(line)?.trim();
        if s == "append" {
            return Ok(XUpdateRange::Append);
        }
        if !s.starts_with("bytes=") {
            Err(typed_headers::Error::Header)?;
        }
        s = &s[6..];

        let nums = s.split("-").collect::<Vec<&str>>();
        if nums.len() != 2 {
            Err(typed_headers::Error::Header)?;
        }
        if nums[0] != "" && nums[1] != "" {
            return Ok(XUpdateRange::FromTo(
                (nums[0]).parse::<u64>().map_err(|_|typed_headers::Error::Header)?,
                (nums[1]).parse::<u64>().map_err(|_|typed_headers::Error::Header)?,
            ));
        }
        if nums[0] != "" {
            return Ok(XUpdateRange::AllFrom((nums[0]).parse::<u64>()
                                        .map_err(|_|typed_headers::Error::Header)?));
        }
        if nums[1] != "" {
            return Ok(XUpdateRange::Last((nums[1]).parse::<u64>()
                                        .map_err(|_|typed_headers::Error::Header)?));
        }
        return Err(typed_headers::Error::Header);
    }

    fn fmt_header(&self, f: &mut typed_headers::Formatter) -> fmt::Result {
        let value = match self {
            &XUpdateRange::Append => "append".to_string(),
            &XUpdateRange::FromTo(b, e) => format!("{}-{}", b, e),
            &XUpdateRange::AllFrom(b) => format!("{}-", b),
            &XUpdateRange::Last(e) => format!("-{}", e),
        };
        f.fmt_line(&value)
    }
}

#[derive(Debug,Clone,PartialEq)]
pub struct ContentRange(pub u64, pub u64);

impl Header for ContentRange {
    fn header_name() -> &'static str {
        "Content-Range"
    }

    fn parse_header<'a, T: RawLike<'a>>(raw: &'a T) -> typed_headers::Result<ContentRange> {
        let line = raw.one().ok_or(typed_headers::Error::Header)?;
        let mut s = std::str::from_utf8(line)?.trim();
        if !s.starts_with("bytes") {
            Err(typed_headers::Error::Header)?;
        }
        s = &s[6..];

        let noslash = s.split("/").collect::<Vec<&str>>();
        let nums = noslash[0].split("-").collect::<Vec<&str>>();
        if nums.len() != 2 {
            Err(typed_headers::Error::Header)?;
        }
        return Ok(ContentRange(
            (nums[0]).parse::<u64>().map_err(|_|typed_headers::Error::Header)?,
            (nums[1]).parse::<u64>().map_err(|_|typed_headers::Error::Header)?,
        ));
    }

    fn fmt_header(&self, f: &mut typed_headers::Formatter) -> fmt::Result {
        f.fmt_line(&format!("{}-{}", self.0, self.1))
    }
}

// The "If" header contains IfLists, of which the results are ORed.
#[derive(Debug,Clone,PartialEq)]
pub struct If(pub Vec<IfList>);

// An IfList contains Conditions, of which the results are ANDed.
#[derive(Debug,Clone,PartialEq)]
pub struct IfList {
    pub resource_tag:   Option<url::Url>,
    pub conditions:     Vec<IfCondition>,
}

// helpers.
impl IfList {
    fn new() -> IfList {
        IfList{
            resource_tag: None,
            conditions: Vec::new(),
        }
    }
    fn add(&mut self, not: bool, item: IfItem) {
        self.conditions.push({IfCondition{not, item}});
    }
}

// Single Condition is [NOT] State-Token | EntityTag
#[derive(Debug,Clone,PartialEq)]
pub struct IfCondition {
    pub not:    bool,
    pub item:   IfItem,
}
#[derive(Debug,Clone,PartialEq)]
pub enum IfItem {
    StateToken(String),
    ETag(EntityTag),
}

// Below stuff is for the parser state.
#[derive(Debug,Clone,PartialEq)]
enum IfToken {
    ListOpen,
    ListClose,
    Not,
    Word(String),
    Pointy(String),
    ETag(EntityTag),
    End,
}

#[derive(Debug,Clone,PartialEq)]
enum IfState {
    Start,
    RTag,
    List,
    Not,
    Bad,
}

// helpers.
fn is_whitespace(c: u8) -> bool { b" \t\r\n".iter().any(|&x| x == c) }
fn is_special(c: u8) -> bool { b"<>()[]".iter().any(|&x| x == c) }

fn trim_left<'a>(mut out: &'a[u8]) -> &'a[u8] {
    while !out.is_empty() && is_whitespace(out[0]) {
        out = &out[1..];
    }
    out
}

// parse one token.
fn scan_until(buf: &[u8], c: u8) -> typed_headers::Result<(&[u8], &[u8])> {
    let mut i = 1;
    let mut quote = false;
    while quote || buf[i] != c {
        if buf.is_empty() || is_whitespace(buf[i]) {
            return Err(typed_headers::Error::Header);
        }
        if buf[i] == b'"' {
            quote = !quote;
        }
        i += 1
    }
    Ok((&buf[1..i], &buf[i+1..]))
}

// scan one word.
fn scan_word(buf: &[u8]) -> typed_headers::Result<(&[u8], &[u8])> {
    for (i, &c) in buf.iter().enumerate() {
        if is_whitespace(c) || is_special(c) || c < 32 {
            if i == 0 {
                return Err(typed_headers::Error::Header);
            }
            return Ok((&buf[..i], &buf[i..]));
        }
    }
    Ok((buf, b""))
}

// get next token.
fn get_token<'a>(buf: &'a[u8]) -> typed_headers::Result<(IfToken, &'a[u8])> {
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
            let s = std::string::String::from_utf8(tok.to_vec())?;
            Ok((IfToken::Pointy(s), rest))
        }
        b'[' => {
            let (tok, rest) = scan_until(buf, b']')?;
            let s = std::str::from_utf8(tok)?;
            Ok((IfToken::ETag(EntityTag::from_str(s)?), rest))
        },
        _ => {
            let (tok, rest) = scan_word(buf)?;
            if tok == b"Not" {
                Ok((IfToken::Not, rest))
            } else {
                let s = std::string::String::from_utf8(tok.to_vec())?;
                Ok((IfToken::Word(s), rest))
            }
        }
    }
}

impl Header for If {
    fn header_name() -> &'static str {
        "If"
    }

    fn parse_header<'a, T: RawLike<'a>>(raw: &'a T) -> typed_headers::Result<If> {

        // one big state machine.
        let mut if_lists = If(Vec::new());
        let mut cur_list = IfList::new();

        let mut state = IfState::Start;
        let mut input = raw.one().ok_or(typed_headers::Error::Header)?;

        loop {
            let (tok, rest) = get_token(input)?;
            input = rest;
            state = match state {
                IfState::Start => {
                    match tok {
                        IfToken::ListOpen => IfState::List,
                        IfToken::Pointy(url) => {
                            let u = url::Url::parse(&url).map_err(|_| typed_headers::Error::Header)?;
                            cur_list.resource_tag = Some(u);
                            IfState::RTag
                        },
                        IfToken::End => {
                            if if_lists.0.len() > 0 {
                                break;
                            }
                            IfState::Bad
                        },
                        _ => IfState::Bad,
                    }
                },
                IfState::RTag => {
                    match tok {
                        IfToken::ListOpen => IfState::List,
                        _ => IfState::Bad,
                    }
                },
                IfState::List |
                IfState::Not => {
                    let not = state == IfState::Not;
                    match tok {
                        IfToken::Not => {
                            if not { IfState::Bad } else { IfState::Not }
                        },
                        IfToken::Pointy(stok) |
                        IfToken::Word(stok) => {
                            // as we don't have an URI parser, just
                            // check if there's at least one ':' in there.
                            if !stok.contains(":") {
                                IfState::Bad
                            } else {
                                cur_list.add(not, IfItem::StateToken(stok));
                                IfState::List
                            }
                        }
                        IfToken::ETag(etag) => {
                            cur_list.add(not, IfItem::ETag(etag));
                            IfState::List
                        },
                        IfToken::ListClose => {
                            if cur_list.conditions.is_empty() {
                                IfState::Bad
                            } else {
                                if_lists.0.push(cur_list);
                                cur_list = IfList::new();
                                IfState::Start
                            }
                        },
                        _ => IfState::Bad,
                    }
                },
                IfState::Bad => return Err(typed_headers::Error::Header),
            };
        }
        Ok(if_lists)
    }

    fn fmt_header(&self, f: &mut typed_headers::Formatter) -> fmt::Result {
        f.fmt_line(&format!("[If header]"))
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn if_header() {
        use super::*;
        // Note that some implementations (golang net/x/webdav) also
        // accept a "plain  word" as StateToken, instead of only
        // a Coded-Url (<...>). We allow that as well, but I have
        // no idea if we need to (or should!).
        let val = br#"  <http://x.yz/> ([W/"etag"] Not <DAV:nope> )
            (Not<urn:x>[W/"bla"] plain:word:123) "#;
        let hdr = If::parse_header(&vec![val.to_vec()]);
        assert!(hdr.is_ok());
    }
}

