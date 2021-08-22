use std::io;

use futures_util::{Stream, StreamExt};

use http::{Response, StatusCode};
use xml;
use xml::common::XmlVersion;
use xml::writer::EventWriter;
use xml::writer::XmlEvent as XmlWEvent;
use xml::EmitterConfig;

use crate::async_stream::AsyncStream;
use crate::body::Body;
use crate::davpath::DavPath;
use crate::util::MemBuffer;
use crate::DavError;

type Sender = crate::async_stream::Sender<(DavPath, StatusCode), DavError>;

pub(crate) struct MultiError(Sender);

impl MultiError {
    pub fn new(sender: Sender) -> MultiError {
        MultiError(sender)
    }

    pub async fn add_status<'a>(
        &'a mut self,
        path: &'a DavPath,
        status: impl Into<DavError> + 'static,
    ) -> Result<(), futures_channel::mpsc::SendError>
    {
        let status = status.into().statuscode();
        self.0.send((path.clone(), status)).await;
        Ok(())
    }
}

type XmlWriter<'a> = EventWriter<MemBuffer>;

fn write_elem<'b, S>(xw: &mut XmlWriter, name: S, text: &str) -> Result<(), DavError>
where S: Into<xml::name::Name<'b>> {
    let n = name.into();
    xw.write(XmlWEvent::start_element(n))?;
    if text.len() > 0 {
        xw.write(XmlWEvent::characters(text))?;
    }
    xw.write(XmlWEvent::end_element())?;
    Ok(())
}

fn write_response(mut w: &mut XmlWriter, path: &DavPath, sc: StatusCode) -> Result<(), DavError> {
    w.write(XmlWEvent::start_element("D:response"))?;
    let p = path.with_prefix().as_url_string();
    write_elem(&mut w, "D:href", &p)?;
    write_elem(&mut w, "D:status", &format!("HTTP/1.1 {}", sc))?;
    w.write(XmlWEvent::end_element())?;
    Ok(())
}

pub(crate) async fn multi_error<S>(req_path: DavPath, status_stream: S) -> Result<Response<Body>, DavError>
where S: Stream<Item = Result<(DavPath, StatusCode), DavError>> + Send + 'static {
    // read the first path/status item
    let mut status_stream = Box::pin(status_stream);
    let (path, status) = match status_stream.next().await {
        None => {
            debug!("multi_error: empty status_stream");
            return Err(DavError::ChanError);
        },
        Some(Err(e)) => return Err(e),
        Some(Ok(item)) => item,
    };

    let mut items = Vec::new();

    if path == req_path {
        // the first path/status item was for the request path.
        // see if there is a next item.
        match status_stream.next().await {
            None => {
                // No, this was the first and only item.
                let resp = Response::builder().status(status).body(Body::empty()).unwrap();
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
    let body = AsyncStream::new(|mut tx| {
        async move {
            // Write initial header.
            let mut xw = EventWriter::new_with_config(
                MemBuffer::new(),
                EmitterConfig {
                    perform_indent: true,
                    ..EmitterConfig::default()
                },
            );
            xw.write(XmlWEvent::StartDocument {
                version:    XmlVersion::Version10,
                encoding:   Some("utf-8"),
                standalone: None,
            })
            .map_err(DavError::from)?;
            xw.write(XmlWEvent::start_element("D:multistatus").ns("D", "DAV:"))
                .map_err(DavError::from)?;
            let data = xw.inner_mut().take();
            tx.send(data).await;

            // now write the items.
            let mut status_stream = futures_util::stream::iter(items).chain(status_stream);
            while let Some(res) = status_stream.next().await {
                let (path, status) = res?;
                let status = if status == StatusCode::NO_CONTENT {
                    StatusCode::OK
                } else {
                    status
                };
                write_response(&mut xw, &path, status)?;
                let data = xw.inner_mut().take();
                tx.send(data).await;
            }

            // and finally write the trailer.
            xw.write(XmlWEvent::end_element()).map_err(DavError::from)?;
            let data = xw.inner_mut().take();
            tx.send(data).await;

            Ok::<_, io::Error>(())
        }
    });

    // return response.
    let resp = Response::builder()
        .header("content-type", "application/xml; charset=utf-8")
        .status(StatusCode::MULTI_STATUS)
        .body(Body::from(body))
        .unwrap();
    Ok(resp)
}
