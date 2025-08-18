use std::io;

use actix_web::{App, HttpServer, web};
use dav_server::actix::*;
use dav_server::{DavConfig, DavHandler, fakels::FakeLs, localfs::LocalFs};

pub async fn dav_handler(req: DavRequest, davhandler: web::Data<DavHandler>) -> DavResponse {
    if let Some(prefix) = req.prefix() {
        let config = DavConfig::new().strip_prefix(prefix);
        davhandler.handle_with(config, req.request).await.into()
    } else {
        davhandler.handle(req.request).await.into()
    }
}

#[actix_web::main]
async fn main() -> io::Result<()> {
    env_logger::init();
    let addr = "127.0.0.1:4918";
    let dir = "/tmp";

    let dav_server = DavHandler::builder()
        .filesystem(LocalFs::new(dir, false, false, false))
        .locksystem(FakeLs::new())
        .build_handler();

    println!("actix-web example: listening on {} serving {}", addr, dir);

    HttpServer::new(move || {
        App::new()
            .app_data(web::Data::new(dav_server.clone()))
            .service(web::resource("/{tail:.*}").to(dav_handler))
    })
    .bind(addr)?
    .run()
    .await
}
