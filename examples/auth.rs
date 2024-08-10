use std::{convert::Infallible, fmt::Display, net::SocketAddr, path::Path};

use futures_util::{stream, StreamExt};
use http::{Request, Response, StatusCode};
use hyper::{body::Incoming, server::conn::http1, service::service_fn};
use hyper_util::rt::TokioIo;
use tokio::{net::TcpListener, task::spawn};

use dav_server::{
    body::Body,
    davpath::DavPath,
    fakels::FakeLs,
    fs::{
        DavDirEntry, DavFile, DavMetaData, FsFuture, FsResult, FsStream, GuardedFileSystem,
        OpenOptions, ReadDirMeta,
    },
    localfs::LocalFs,
    DavHandler,
};

/// The server example demonstrates a limited scope policy for access to the file system.
/// Depending on the filter specified by the user in the request, one will receive only files or directories.
/// For example, try this URLs:
/// - dav://dirs:-@127.0.0.1:4918 — responds only with directories.
/// - dav://files:-@127.0.0.1:4918 — responds only with files.
#[tokio::main]
async fn main() {
    env_logger::init();
    let dir = "/tmp";
    let addr: SocketAddr = ([127, 0, 0, 1], 4918).into();
    let fs = FilteredFs::new(dir);
    let dav_server = DavHandler::builder()
        .filesystem(Box::new(fs) as _)
        .locksystem(FakeLs::new())
        .build_handler();
    let listener = TcpListener::bind(addr).await.unwrap();
    println!("Listening {addr}");
    loop {
        let (stream, _client_addr) = listener.accept().await.unwrap();
        let dav_server = dav_server.clone();
        let io = TokioIo::new(stream);
        spawn(async move {
            let service = service_fn(move |request| handle(request, dav_server.clone()));
            if let Err(err) = http1::Builder::new().serve_connection(io, service).await {
                eprintln!("Failed serving: {err:?}");
            }
        });
    }
}

async fn handle(
    request: Request<Incoming>,
    handler: DavHandler<Filter>,
) -> Result<Response<Body>, Infallible> {
    /// https://developer.mozilla.org/en-US/docs/Web/HTTP/Headers/WWW-Authenticate
    static AUTH_CHALLENGE: &str = "Basic realm=\"Specify the directory entries' filter \
        as a username: `dirs`, `files` or `all`; password — any string\"";
    let filter = match Filter::from_request(&request) {
        Ok(f) => f,
        Err(err) => {
            let response = Response::builder()
                .status(StatusCode::UNAUTHORIZED)
                .header("WWW-Authenticate", AUTH_CHALLENGE)
                .body(err.to_string().into())
                .expect("Auth error response must be built fine");
            return Ok(response);
        }
    };
    Ok(handler.handle_guarded(request, filter).await)
}

#[derive(Clone)]
struct FilteredFs {
    inner: Box<LocalFs>,
}

impl FilteredFs {
    fn new(dir: impl AsRef<Path>) -> Self {
        Self {
            inner: LocalFs::new(dir, false, false, false),
        }
    }
}

impl GuardedFileSystem<Filter> for FilteredFs {
    fn open<'a>(
        &'a self,
        path: &'a DavPath,
        options: OpenOptions,
        _credentials: &'a Filter,
    ) -> FsFuture<Box<dyn DavFile>> {
        self.inner.open(path, options, &())
    }

    fn read_dir<'a>(
        &'a self,
        path: &'a DavPath,
        meta: ReadDirMeta,
        filter: &'a Filter,
    ) -> FsFuture<FsStream<Box<dyn DavDirEntry>>> {
        Box::pin(async move {
            let mut stream = self.inner.read_dir(path, meta, &()).await?;
            let mut entries = Vec::default();
            while let Some(entry) = stream.next().await {
                let entry = entry?;
                if filter.matches(entry.as_ref()).await? {
                    entries.push(Ok(entry));
                }
            }
            Ok(Box::pin(stream::iter(entries)) as _)
        })
    }

    fn metadata<'a>(
        &'a self,
        path: &'a DavPath,
        _credentials: &'a Filter,
    ) -> FsFuture<Box<dyn DavMetaData>> {
        self.inner.metadata(path, &())
    }
}

#[derive(Clone)]
enum Filter {
    All,
    Files,
    Dirs,
}

impl Filter {
    fn from_request(request: &Request<Incoming>) -> Result<Self, Box<dyn Display>> {
        use headers::{authorization::Basic, Authorization, HeaderMapExt};

        let auth = request
            .headers()
            .typed_get::<Authorization<Basic>>()
            .ok_or(Box::new("please auth") as _)?;
        match auth.username() {
            "all" => Ok(Filter::All),
            "files" => Ok(Filter::Files),
            "dirs" => Ok(Filter::Dirs),
            _ => Err(Box::new("unexpected filter value") as _),
        }
    }

    async fn matches(&self, entry: &dyn DavDirEntry) -> FsResult<bool> {
        if let Filter::All = self {
            return Ok(true);
        }
        Ok(entry.is_dir().await? == matches!(self, Filter::Dirs))
    }
}
