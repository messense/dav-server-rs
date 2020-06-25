use std::io;

use actix_web::{web, App, HttpServer};
use webdav_handler::{DavHandler, DavConfig, localfs::LocalFs, fakels::FakeLs};
use webdav_handler::actix::*;

pub async fn dav_handler(req: DavRequest, davhandler: web::Data<DavHandler>) -> DavResponse {
    if let Some(prefix) = req.prefix() {
        let config = DavConfig::new().strip_prefix(prefix);
        davhandler.handle_with(config, req.request).await.into()
    } else {
        davhandler.handle(req.request).await.into()
    }
}

#[actix_rt::main]
async fn main() -> io::Result<()> {
    env_logger::init();
    let addr = "127.0.0.1:4918";
    let dir = "/tmp";

    let dav_server = DavHandler::builder()
        .filesystem(LocalFs::new(dir, false, false, false))
        .locksystem(FakeLs::new())
        .build_handler();

    HttpServer::new(move || App::new().data(dav_server.clone()).service(
        web::resource("/{tail:.*}").to(dav_handler)
    ))
        .bind(addr)?
        .run()
        .await
}
