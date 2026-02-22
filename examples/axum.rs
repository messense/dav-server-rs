use axum::{Extension, Router, extract::Request, response::IntoResponse, routing::any};
use dav_server::{DavHandler, fakels::FakeLs, localfs::LocalFs};
use tokio::net::TcpListener;

fn main() {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async_main());
}

async fn async_main() {
    if std::env::var("RUST_LOG").is_err() {
        unsafe {
            std::env::set_var("RUST_LOG", "debug");
        }
    }
    env_logger::init();
    let ip = "127.0.0.1";
    let port = 4918;

    let addr = format!("{ip}:{port}");
    let listener = TcpListener::bind(&addr).await.unwrap();

    let dav = DavHandler::builder()
        .filesystem(LocalFs::new("/tmp", false, false, false))
        .locksystem(FakeLs::new())
        .strip_prefix("/dav")
        .autoindex(true)
        .hide_symlinks(true)
        .build_handler();

    let router = Router::new()
        .route("/dav", any(handle_dav))
        .route("/dav/", any(handle_dav))
        .route("/dav/{*path}", any(handle_dav))
        .layer(Extension(dav));

    log::info!("serve at http://{addr}");
    axum::serve(listener, router).await.unwrap();
}

async fn handle_dav(Extension(dav): Extension<DavHandler>, req: Request) -> impl IntoResponse {
    dav.handle(req).await
}
