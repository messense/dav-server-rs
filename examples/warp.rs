use std::net::SocketAddr;
use webdav_handler::warp::dav_dir;

#[tokio::main]
async fn main() {
    env_logger::init();
    let dir = "/tmp";
    let addr: SocketAddr = ([127, 0, 0, 1], 4918).into();

    println!("Serving {} on {}", dir, addr);
    let warpdav = dav_dir(dir, true, true);
    warp::serve(warpdav).run(addr).await;
}
