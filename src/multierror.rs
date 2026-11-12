use std::io;

use futures_util::{Stream, StreamExt};

use http::{Response, StatusCode};
use xml::EmitterConfig;
use xml::common::XmlVersion;
use xml::name::Name;
use xml::writer::EventWriter;
use xml::writer::XmlEvent as XmlWEvent;

use crate::DavError;
use crate::async_stream::AsyncStream;
use crate::body::Body;
use crate::davpath::DavPath;
use crate::util::MemBuffer;

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
    ) -> Result<(), futures_channel::mpsc::SendError> {
        let status = status.into().statuscode();
        self.0.send((path.clone(), status)).await;
        Ok(())
    }
}

type XmlWriter<'a> = EventWriter<MemBuffer>;

fn write_elem<'b, S>(xw: &mut XmlWriter, name: S, text: &str) -> Result<(), DavError>
where
    S: Into<xml::name::Name<'b>>,
{
    let n = name.into();
    xw.write(XmlWEvent::start_element(n))?;
    if !text.is_empty() {
        xw.write(XmlWEvent::characters(text))?;
    }
    xw.write(XmlWEvent::end_element())?;
    Ok(())
}

fn write_response(
    w: &mut XmlWriter,
    pfx: &str,
    path: &DavPath,
    sc: StatusCode,
) -> Result<(), DavError> {
    w.write(XmlWEvent::start_element(Name::prefixed("response", pfx)))?;
    let p = path.with_prefix().as_url_string();
    write_elem(w, Name::prefixed("href", pfx), &p)?;
    write_elem(w, Name::prefixed("status", pfx), &format!("HTTP/1.1 {sc}"))?;
    w.write(XmlWEvent::end_element())?;
    Ok(())
}

/// `pfx` is the prefix for the DAV: namespace.
pub(crate) async fn multi_error<S>(
    req_path: DavPath,
    pfx: String,
    status_stream: S,
) -> Result<Response<Body>, DavError>
where
    S: Stream<Item = Result<(DavPath, StatusCode), DavError>> + Send + 'static,
{
    // read the first path/status item
    let mut status_stream = Box::pin(status_stream);
    let (path, status) = match status_stream.next().await {
        None => {
            debug!("multi_error: empty status_stream");
            return Err(DavError::ChanError);
        }
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
                let resp = Response::builder()
                    .status(status)
                    .body(Body::empty())
                    .unwrap();
                return Ok(resp);
            }
            Some(Err(e)) => return Err(e),
            Some(Ok(item)) => {
                // Yes, more than one response.
                items.push(Ok((path, status)));
                items.push(Ok(item));
            }
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
                version: XmlVersion::Version10,
                encoding: Some("utf-8"),
                standalone: None,
            })
            .map_err(DavError::from)?;
            xw.write(
                XmlWEvent::start_element(Name::prefixed("multistatus", &pfx))
                    .ns(pfx.as_str(), "DAV:"),
            )
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
                write_response(&mut xw, &pfx, &path, status)?;
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

#[cfg(test)]
mod tests {
    use futures_util::StreamExt;
    use http::StatusCode;

    use super::multi_error;
    use crate::davpath::DavPath;

    #[tokio::test]
    async fn multi_error_uses_the_dav_prefix() {
        let items = futures_util::stream::iter(vec![Ok((
            DavPath::new("/dir/locked.txt").unwrap(),
            StatusCode::LOCKED,
        ))]);
        let mut resp = multi_error(DavPath::new("/dir/").unwrap(), "d".to_string(), items)
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::MULTI_STATUS);
        let mut body = Vec::new();
        while let Some(chunk) = resp.body_mut().next().await {
            body.extend_from_slice(&chunk.unwrap());
        }
        let body = String::from_utf8(body).unwrap();
        let tree = xmltree::Element::parse(body.as_bytes()).unwrap();
        assert_eq!(tree.namespace.as_deref(), Some("DAV:"), "{body}");
        assert!(body.contains(r#"<d:multistatus xmlns:d="DAV:">"#), "{body}");
        assert!(body.contains("<d:href>/dir/locked.txt</d:href>"), "{body}");
        assert!(
            body.contains("<d:status>HTTP/1.1 423 Locked</d:status>"),
            "{body}"
        );
        assert!(!body.contains("D:"), "{body}");
    }
}
