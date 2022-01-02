use std::convert::Infallible;
use dav_server::{fakels::FakeLs, localfs::LocalFs, DavHandler};

#[tokio::main]
async fn main() {
    env_logger::init();
    let dir = "/tmp";
    let addr = ([127, 0, 0, 1], 4918).into();

    let dav_server = DavHandler::builder()
        .filesystem(LocalFs::new(dir, false, false, false))
        .locksystem(FakeLs::new())
        .build_handler();

    let make_service = hyper::service::make_service_fn(move |_| {
        let dav_server = dav_server.clone();
        async move {
            let func = move |req| {
                let dav_server = dav_server.clone();
                async move { Ok::<_, Infallible>(dav_server.handle(req).await) }
            };
            Ok::<_, Infallible>(hyper::service::service_fn(func))
        }
    });

    println!("hyper example: listening on {:?} serving {}", addr, dir);
    let _ = hyper::Server::bind(&addr)
        .serve(make_service)
        .await
        .map_err(|e| eprintln!("server error: {}", e));
}
