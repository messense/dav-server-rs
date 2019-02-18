use std::error::Error;
use std::io::ErrorKind;

use xml;
use http::StatusCode;

use crate::fs::FsError;

#[derive(Debug)]
pub(crate) enum DavError {
    XmlReadError,       // error reading/parsing xml
    XmlParseError,      // error interpreting xml
    InvalidPath,        // error parsing path
    IllegalPath,        // path not valid here
    ForbiddenPath,      // too many dotdots
    UnknownMethod,
    Status(StatusCode),
    StatusClose(StatusCode),
    FsError(FsError),
    IoError(std::io::Error),
    XmlReaderError(xml::reader::Error),
    XmlWriterError(xml::writer::Error),
}

impl Error for DavError {
    fn description(&self) -> &str {
        "DAV error"
    }

    fn cause(&self) -> Option<&Error> {
        match self {
            &DavError::FsError(ref e) => Some(e),
            &DavError::IoError(ref e) => Some(e),
            &DavError::XmlReaderError(ref e) => Some(e),
            &DavError::XmlWriterError(ref e) => Some(e),
            _ => None,
        }
    }
}

impl std::fmt::Display for DavError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            &DavError::XmlReaderError(_) => write!(f, "XML parse error"),
            &DavError::XmlWriterError(_) => write!(f, "XML generate error"),
            &DavError::IoError(_) => write!(f, "I/O error"),
            _ => write!(f, "{:?}", self),
        }
    }
}

impl From<FsError> for DavError {
    fn from(e: FsError) -> Self {
        DavError::FsError(e)
    }
}

impl From<FsError> for std::io::Error {
    fn from(e: FsError) -> Self {
        std::io::Error::new(std::io::ErrorKind::Other, e)
    }
}

impl From<DavError> for std::io::Error {
    fn from(e: DavError) -> Self {
        std::io::Error::new(std::io::ErrorKind::Other, e)
    }
}

impl From<std::io::Error> for DavError {
    fn from(e: std::io::Error) -> Self {
        DavError::IoError(e)
    }
}

impl From<xml::reader::Error> for DavError {
    fn from(e: xml::reader::Error) -> Self {
        DavError::XmlReaderError(e)
    }
}

impl From<xml::writer::Error> for DavError {
    fn from(e: xml::writer::Error) -> Self {
        DavError::XmlWriterError(e)
    }
}

fn ioerror_to_status(ioerror: &std::io::Error) -> StatusCode {
    match ioerror.kind() {
        ErrorKind::NotFound => StatusCode::NOT_FOUND,
        ErrorKind::PermissionDenied => StatusCode::FORBIDDEN,
        ErrorKind::AlreadyExists => StatusCode::CONFLICT,
        ErrorKind::TimedOut => StatusCode::GATEWAY_TIMEOUT,
        _ => StatusCode::BAD_GATEWAY,
    }
}

fn fserror_to_status(e: &FsError) -> StatusCode {
    match e {
        FsError::NotImplemented => StatusCode::NOT_IMPLEMENTED,
        FsError::GeneralFailure => StatusCode::INTERNAL_SERVER_ERROR,
        FsError::Exists => StatusCode::METHOD_NOT_ALLOWED,
        FsError::NotFound => StatusCode::NOT_FOUND,
        FsError::Forbidden => StatusCode::FORBIDDEN,
        FsError::InsufficientStorage => StatusCode::INSUFFICIENT_STORAGE,
        FsError::LoopDetected => StatusCode::LOOP_DETECTED,
        FsError::PathTooLong => StatusCode::URI_TOO_LONG,
        FsError::TooLarge => StatusCode::PAYLOAD_TOO_LARGE,
        FsError::IsRemote => StatusCode::BAD_GATEWAY,
    }
}

impl DavError {
    pub(crate) fn statuscode(&self) -> StatusCode {
        match self {
            &DavError::XmlReadError => StatusCode::BAD_REQUEST,
            &DavError::XmlParseError => StatusCode::BAD_REQUEST,
            &DavError::InvalidPath => StatusCode::BAD_REQUEST,
            &DavError::IllegalPath => StatusCode::BAD_GATEWAY,
            &DavError::ForbiddenPath => StatusCode::FORBIDDEN,
            &DavError::UnknownMethod => StatusCode::NOT_IMPLEMENTED,
            &DavError::IoError(ref e) => ioerror_to_status(e),
            &DavError::FsError(ref e) => fserror_to_status(e),
            &DavError::Status(e) => e,
            &DavError::StatusClose(e) => e,
            &DavError::XmlReaderError(ref _e) => StatusCode::BAD_REQUEST,
            &DavError::XmlWriterError(ref _e) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    pub(crate) fn must_close(&self) -> bool {
        match self {
            &DavError::Status(_) => false,
            _ => true,
        }
    }
}

