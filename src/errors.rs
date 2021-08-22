use std::error::Error;
use std::io::{self, ErrorKind};

use http::StatusCode;
use xml;

use crate::fs::FsError;

pub(crate) type DavResult<T> = Result<T, DavError>;

#[derive(Debug)]
pub(crate) enum DavError {
    XmlReadError,  // error reading/parsing xml
    XmlParseError, // error interpreting xml
    InvalidPath,   // error parsing path
    IllegalPath,   // path not valid here
    ForbiddenPath, // too many dotdots
    UnknownDavMethod,
    ChanError,
    Utf8Error,
    Status(StatusCode),
    StatusClose(StatusCode),
    FsError(FsError),
    IoError(io::Error),
    XmlReaderError(xml::reader::Error),
    XmlWriterError(xml::writer::Error),
}

impl Error for DavError {
    fn description(&self) -> &str {
        "DAV error"
    }

    fn cause(&self) -> Option<&dyn Error> {
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

impl From<DavError> for io::Error {
    fn from(e: DavError) -> Self {
        match e {
            DavError::IoError(e) => e,
            DavError::FsError(e) => e.into(),
            _ => io::Error::new(io::ErrorKind::Other, e),
        }
    }
}

impl From<FsError> for io::Error {
    fn from(e: FsError) -> Self {
        fserror_to_ioerror(e)
    }
}

impl From<io::Error> for DavError {
    fn from(e: io::Error) -> Self {
        DavError::IoError(e)
    }
}

impl From<StatusCode> for DavError {
    fn from(e: StatusCode) -> Self {
        DavError::Status(e)
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

impl From<std::str::Utf8Error> for DavError {
    fn from(_: std::str::Utf8Error) -> Self {
        DavError::Utf8Error
    }
}

impl From<std::string::FromUtf8Error> for DavError {
    fn from(_: std::string::FromUtf8Error) -> Self {
        DavError::Utf8Error
    }
}

impl From<futures_channel::mpsc::SendError> for DavError {
    fn from(_e: futures_channel::mpsc::SendError) -> Self {
        DavError::ChanError
    }
}

fn fserror_to_ioerror(e: FsError) -> io::Error {
    match e {
        FsError::NotImplemented => io::Error::new(io::ErrorKind::Other, "NotImplemented"),
        FsError::GeneralFailure => io::Error::new(io::ErrorKind::Other, "GeneralFailure"),
        FsError::Exists => io::Error::new(io::ErrorKind::AlreadyExists, "Exists"),
        FsError::NotFound => io::Error::new(io::ErrorKind::NotFound, "Notfound"),
        FsError::Forbidden => io::Error::new(io::ErrorKind::PermissionDenied, "Forbidden"),
        FsError::InsufficientStorage => io::Error::new(io::ErrorKind::Other, "InsufficientStorage"),
        FsError::LoopDetected => io::Error::new(io::ErrorKind::Other, "LoopDetected"),
        FsError::PathTooLong => io::Error::new(io::ErrorKind::Other, "PathTooLong"),
        FsError::TooLarge => io::Error::new(io::ErrorKind::Other, "TooLarge"),
        FsError::IsRemote => io::Error::new(io::ErrorKind::Other, "IsRemote"),
    }
}

fn ioerror_to_status(ioerror: &io::Error) -> StatusCode {
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
            &DavError::UnknownDavMethod => StatusCode::NOT_IMPLEMENTED,
            &DavError::ChanError => StatusCode::INTERNAL_SERVER_ERROR,
            &DavError::Utf8Error => StatusCode::UNSUPPORTED_MEDIA_TYPE,
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
            // non-fatal, keep the connnection open.
            &DavError::Status(_) |
            &DavError::FsError(FsError::NotFound) |
            &DavError::FsError(FsError::Forbidden) |
            &DavError::FsError(FsError::Exists) => false,
            // close the connection to be sure.
            _ => true,
        }
    }
}
