use actix_web::{server, App, AsyncResponder, Error, HttpMessage, HttpRequest, HttpResponse};
use failure::Fail;
use futures01::{future, Future, Stream};
use http;
use webdav_handler::{fakels::FakeLs, localfs::LocalFs, DavHandler};

use threadpool_helper::{pipe_from_threadpool, pipe_to_threadpool, spawn_on_threadpool};

// Actix-web webdav handler.
fn handle_webdav(httpreq: &HttpRequest<DavHandler>) -> Box<Future<Item = HttpResponse, Error = Error>> {
    // transform the Actix http request into a standard http request.
    let req = httpreq.request();
    let mut builder = http::Request::builder();
    builder.method(req.method().to_owned());
    builder.uri(req.uri().to_owned());
    builder.version(req.version().to_owned());
    for (name, value) in req.headers().iter() {
        builder.header(name, value);
    }
    let body = pipe_to_threadpool(req.payload().map_err(|e| e.compat()));
    let std_req = builder.body(body).unwrap();

    // generate the handler-future.
    let davhandler = httpreq.state().clone();
    let std_resp = spawn_on_threadpool(future::lazy(move || davhandler.handle(std_req)));

    // transform the standard http response into an Actix HttpResponse.
    std_resp
        .and_then(|resp| {
            let (parts, body) = resp.into_parts();
            let http::response::Parts {
                status,
                version,
                mut headers,
                ..
            } = parts;
            let mut resp = HttpResponse::build(status);
            for (name, values) in headers.drain() {
                for value in values.into_iter() {
                    resp.header(name.clone(), value);
                }
            }
            Ok(resp
                .version(version)
                .streaming(pipe_from_threadpool(body).map_err(|e| Error::from(e)))
                .into())
        })
        .map_err(|e| e.into())
        .responder()
}

fn main() {
    env_logger::init();
    let _pool = threadpool_helper::init();

    let dir = "/tmp";
    let addr = "127.0.0.1:4918";

    let dav_server = DavHandler::new(None, LocalFs::new(dir, false, false), Some(FakeLs::new()));

    println!("Serving {} on {}", dir, addr);
    server::new(move || App::with_state(dav_server.clone()).default_resource(|r| r.f(handle_webdav)))
        .bind(addr)
        .expect("Can not bind to listen port")
        .run();
}

// From https://actix.rs/actix/actix/ :
//
//     At the moment actix uses current_thread runtime.
//     While it provides minimum overhead, it has its own limits:
//
//     - You cannot use tokio's async file I/O, as it relies on blocking calls
//       that are not available in current_thread
//
// The mod below adds some helpers that do make it possible to use
// code that uses tokio_threadpool::blocking(), such as webdav_handler,
// in combination with actix-web.
//
mod threadpool_helper {

    use std::cell::RefCell;
    use std::sync::Mutex;

    use futures01::sync::{mpsc, oneshot, oneshot::SpawnHandle};
    use futures01::{future, Future, Stream};
    use lazy_static::lazy_static;
    use tokio_current_thread::TaskExecutor;
    use tokio_threadpool;

    // G_SENDER holds a global tokio_threadpool handle.
    lazy_static! {
        static ref G_SENDER: Mutex<Option<tokio_threadpool::Sender>> = Mutex::new(None);
    }

    // T_SENDER holds a thread-local tokio_threadpool handle,
    // cloned from the global one.
    thread_local! {
        static T_SENDER: RefCell<tokio_threadpool::Sender> = RefCell::new(clone_sender());
    }

    // Helper for T_SENDER to clone G_SENDER.
    fn clone_sender() -> tokio_threadpool::Sender {
        let guard = G_SENDER.lock().unwrap();
        guard.as_ref().unwrap().clone()
    }

    // Spawn a future on the tokio_threadpool.
    pub fn spawn_on_threadpool<F>(future: F) -> SpawnHandle<F::Item, F::Error>
    where
        F: Future + Send + 'static,
        F::Item: Send + 'static,
        F::Error: Send + 'static,
    {
        T_SENDER.with(move |sender| oneshot::spawn(future, &*sender.borrow()))
    }

    pub fn pipe_to_threadpool<I, E, S>(strm: S) -> impl Stream<Item = I, Error = E>
    where
        I: 'static,
        E: std::error::Error + 'static,
        S: Stream<Item = I, Error = E> + 'static,
    {
        let (tx, rx) = mpsc::channel(1);
        let fwd = strm
            .then(|res| future::ok::<_, mpsc::SendError<_>>(res))
            .forward(tx)
            .map(|_| ())
            .map_err(|_| ());
        let mut executor = TaskExecutor::current();
        let _ = executor.spawn_local(Box::new(fwd));
        rx.then(|res| future::result(res.unwrap()))
    }

    pub fn pipe_from_threadpool<I, E, S>(strm: S) -> impl Stream<Item = I, Error = E>
    where
        I: 'static + Send,
        E: std::error::Error + 'static + Send,
        S: Stream<Item = I, Error = E> + 'static + Send,
    {
        let (tx, rx) = mpsc::channel(1);
        let fwd = strm
            .then(|res| future::ok::<_, mpsc::SendError<_>>(res))
            .forward(tx)
            .map(|_| ())
            .map_err(|_| ());
        let handle = spawn_on_threadpool(fwd);
        handle.forget();
        rx.then(|res| future::result(res.unwrap()))
    }

    pub fn init() -> tokio_threadpool::ThreadPool {
        let pool = tokio_threadpool::ThreadPool::new();
        {
            let mut guard = G_SENDER.lock().unwrap();
            *guard = Some(pool.sender().clone());
        }
        pool
    }
}
