
use std::cell::RefCell;
use std::io::Write;
use std::rc::Rc;

use futures::prelude::*;
use futures03::sink::SinkExt;
use futures03::stream::Stream as Stream03;
use futures03::stream::StreamExt;

use bytes::Bytes;
use http::{Response,StatusCode};
use xml;
use xml::EmitterConfig;
use xml::common::XmlVersion;
use xml::writer::EventWriter;
use xml::writer::XmlEvent as XmlWEvent;

use crate::{BoxedByteStream,DavError,empty_body};
use crate::makestream;
use crate::webpath::WebPath;

#[derive(Clone)]
pub(crate) struct MultiError(futures03::channel::mpsc::Sender<Result<(WebPath, StatusCode), DavError>>);

impl MultiError {
    pub fn new(sender: futures03::channel::mpsc::Sender<Result<(WebPath, StatusCode), DavError>>)
        -> MultiError
    {
        MultiError(sender)
    }

    pub async fn add_status<'a>(&'a mut self, path: &'a WebPath, status: impl Into<DavError> + 'static)
        -> Result<(), futures03::channel::mpsc::SendError>
    {
        let status = status.into().statuscode();
        await!(self.0.send(Ok((path.clone(), status))))
    }
}

// A buffer that implements "Write".
#[derive(Clone)]
struct MultiBuf(Rc<RefCell<Vec<u8>>>);

impl MultiBuf {
    fn new() -> MultiBuf {
        MultiBuf(Rc::new(RefCell::new(Vec::new())))
    }

    fn take(&self) -> Result<Bytes, DavError> {
        Ok(self.0.replace(Vec::new()).into())
    }
}
unsafe impl Send for MultiBuf {}
unsafe impl Sync for MultiBuf {}

impl Write for MultiBuf {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let len = buf.len();
        self.0.borrow_mut().extend_from_slice(buf);
        Ok(len)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

type XmlWriter = EventWriter<MultiBuf>;

fn write_elem<'b, S>(xw: &mut XmlWriter, name: S, text: &str) -> Result<(), DavError> where S: Into<xml::name::Name<'b>> {
    let n = name.into();
	xw.write(XmlWEvent::start_element(n))?;
    if text.len() > 0 {
        xw.write(XmlWEvent::characters(text))?;
    }
    xw.write(XmlWEvent::end_element())?;
    Ok(())
}

fn write_response(mut w: &mut XmlWriter, path: &WebPath, sc: StatusCode) -> Result<(), DavError> {
    w.write(XmlWEvent::start_element("D:response"))?;
    let p = path.as_url_string_with_prefix();
    write_elem(&mut w, "D:href", &p)?;
    write_elem(&mut w, "D:status", &format!("HTTP/1.1 {}", sc))?;
    w.write(XmlWEvent::end_element())?;
    Ok(())
}

pub(crate) async fn multi_error<S>(req_path: WebPath, status_stream: S)
    -> Result<Response<BoxedByteStream>, DavError>
    where S: Stream03<Item=Result<(WebPath, StatusCode), DavError>> + Send + 'static
{
    // read the first path/status item
    let mut status_stream = Box::pin(status_stream);
    let (path, status) = match await!(status_stream.next()) {
        None => return Err(DavError::ChanSendError),
        Some(Err(e)) => return Err(e),
        Some(Ok(item)) => item,
    };

    let mut items = Vec::new();

    if path == req_path {
        // the first path/status item was for the request path.
        // see if there is a next item.
        match await!(status_stream.next()) {
            None => {
                // No, this was the first and only item.
                let resp = Response::builder()
                    .status(status)
                    .body(empty_body()).unwrap();
                return Ok(resp);
            },
            Some(Err(e)) => return Err(e),
            Some(Ok(item)) => {
                // Yes, more than one response.
                items.push(Ok((path, status)));
                items.push(Ok(item));
            },
        }
    } else {
        items.push(Ok((path, status)));
    }

    // Transform path/status items to XML.
    let body = makestream::stream01(async move |mut tx| {

        // Write initial header.
        let buffer = MultiBuf::new();
        let mut xw = EventWriter::new_with_config(
            buffer.clone(),
            EmitterConfig {
                perform_indent: true,
                ..EmitterConfig::default()
            }
        );
        xw.write(XmlWEvent::StartDocument {
            version: XmlVersion::Version10,
            encoding: Some("utf-8"),
            standalone: None,
        })?;
        xw.write(XmlWEvent::start_element("D:multistatus").ns("D", "DAV:"))?;
        await!(tx.send(buffer.take()))?;

        // now write the items.
        let mut status_stream = futures03::stream::iter(items).chain(status_stream);
        while let Some(res) = await!(status_stream.next()) {
            let (path, status) = res?;
            write_response(&mut xw, &path, status)?;
            await!(tx.send(buffer.take()))?;
        }

        // and finally write the trailer.
        xw.write(XmlWEvent::end_element())?;
        await!(tx.send(buffer.take()))?;

        Ok(())
    });

    // return response.
    let body: BoxedByteStream = Box::new(body.map_err(|e| e.into()));
    let resp = Response::builder()
        .header("content-type", "application/xml; charset=utf-8")
        .status(StatusCode::MULTI_STATUS)
        .body(body)
        .unwrap();
    Ok(resp)
}

