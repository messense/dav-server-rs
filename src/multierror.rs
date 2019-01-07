
use std::io::Write;
use std::io::BufWriter;

use http::StatusCode;

use xml;
use xml::EmitterConfig;
use xml::common::XmlVersion;
use xml::writer::EventWriter;
use xml::writer::XmlEvent as XmlWEvent;

use crate::DavError;
use crate::webpath::WebPath;
use crate::sync_adapter::Response;

type XmlWriter = EventWriter<BufWriter<Response>>;

enum State {
    Fresh(Response),
    Writer(XmlWriter),
    Empty,
}

pub(crate) struct MultiError {
    wstate:     State,
    path:       WebPath,
    respstatus: StatusCode,
}

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

//
// Create  a new MultiError.
//
impl MultiError {

    pub fn new(res: Response, path: &WebPath) -> MultiError {
        MultiError {
            wstate:     State::Fresh(res),
            respstatus: StatusCode::FAILED_DEPENDENCY,
            path:       path.clone(),
        }
    }

    pub fn add_status(&mut self, path: &WebPath, sc: StatusCode) -> Result<(), DavError> {
        let mut wstate = State::Empty;
        std::mem::swap(&mut wstate, &mut self.wstate);
        self.wstate = match wstate {
            State::Fresh(mut res) => {
                if path == &self.path {
                    self.respstatus = sc;
                    *res.status_mut() = self.respstatus;
                    return Ok(());
                }

                let contenttype = "application/xml; charset=utf-8".parse().unwrap();
                res.headers_mut().insert("content-type", contenttype);

                self.respstatus = StatusCode::MULTI_STATUS;
                *res.status_mut() = self.respstatus;
                let res = res.start();

                let bufwriter = BufWriter::new(res);
                // let mut xw = EventWriter::new(bufwriter);
                let mut xw = EventWriter::new_with_config(
                            bufwriter,
                            EmitterConfig {
                                perform_indent: true,
                                ..Default::default()
                            }
                );
                xw.write(XmlWEvent::StartDocument {
                    version: XmlVersion::Version10,
                    encoding: Some("utf-8"),
                    standalone: None,
                })?;

                xw.write(XmlWEvent::start_element("D:multistatus")
                    .ns("D", "DAV:"))?;
                write_response(&mut xw, path, sc)?;

                State::Writer(xw)
            },
            State::Writer(mut xw) => {
                write_response(&mut xw, path, sc)?;
                State::Writer(xw)
            },
            State::Empty => {
                State::Empty
            }
        };
        Ok(())
    }

    pub fn finalstatus(self, path: &WebPath, sc: StatusCode) -> Result<(), DavError> {
        match self.wstate {
            State::Fresh(mut res) => {
                *res.status_mut() = sc;
            },
            State::Writer(mut xw) => {
                write_response(&mut xw, path, sc)?;
                xw.write(XmlWEvent::end_element())?;
                xw.into_inner().flush()?;
            },
            State::Empty => {},
        }
        if sc.is_success() {
            Ok(())
        } else {
            Err(DavError::Status(sc))
        }
    }

    pub fn close(self) -> Result<StatusCode, DavError> {
        if let State::Writer(mut xw) = self.wstate {
            xw.write(XmlWEvent::end_element())?;
            xw.into_inner().flush()?;
        };
        Ok(self.respstatus)
    }
}

