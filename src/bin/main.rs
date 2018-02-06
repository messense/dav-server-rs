//
//  Sample application.
//
//  Listens on localhost:4918, plain http, no ssl.
//  Connect to http://localhost:4918/<DIR>/
//
//  <DIR>   username    password    filesystem
//  public  -           -           <crate>/davdata/public
//  mike    mike        mike        <crate>/davdata/mike
//  simon   simon       simon       <crate>/davdata/simon 
//

#[macro_use]
extern crate hyper;
extern crate log;
extern crate env_logger;
extern crate webdav_lib;

use hyper::header::{Authorization, Basic};
use hyper::server::{Handler,Request, Response};
use hyper::status::StatusCode;

use webdav_lib as dav;
use dav::DavHandler;
use dav::localfs;

header! { (WWWAuthenticate, "WWW-Authenticate") => [String] }

#[derive(Debug)]
struct DirInfo<'a> {
    dirpath:   &'a str,
    prefix:    &'a str,
}

#[derive(Debug)]
struct Server {}

impl Server {
    pub fn new() -> Self {
        Server{}
    }
}

fn authenticate(req: &Request, res: &mut Response, user: &str, pass: &str) -> bool {
    // we must have a login/pass
    // some nice destructuring going on here eh.
    match req.headers.get::<Authorization<Basic>>() {
        Some(&Authorization(Basic{
                                ref username,
                                password: Some(ref password)
                            }
        )) => {
            user == username && pass == password
        },
        _ => {
            res.headers_mut().set(WWWAuthenticate(
                        "Basic realm=\"webdav-lib\"".to_string()));
            *res.status_mut() = StatusCode::Unauthorized;
            false
        },
    }
}

impl Handler for Server {
    fn handle<'a, 'k>(&'a self, req: Request<'a, 'k>, mut res: Response<'a>) {

        // Get request path.
        // NOTE we no not percent-decode here, if this was a real
        // application that would be bad.
        let path = match req.uri {
            hyper::uri::RequestUri::AbsolutePath(ref s) => s.to_string(),
            _ => {
                *res.status_mut() = StatusCode::BadRequest;
                return;
            }
        };

        // path can start with "/public" (no authentication needed)
        // or "/username". known users are "mike" and "simon".
        let x = path.splitn(3, "/").collect::<Vec<&str>>();
        let dirinfo = match x[1] {
            "public" => DirInfo {
                dirpath: "../davdata/public",
                prefix: "/public",
            },
            "mike" => {
                if !authenticate(&req, &mut res, "mike", "mike") {
                    return;
                }
                DirInfo {
                    dirpath: "../davdata/mike",
                    prefix: "/mike",
                }
            },
            "simon" => {
                if !authenticate(&req, &mut res, "simon", "simon") {
                    return;
                }
                DirInfo {
                    dirpath: "../davdata/simon",
                    prefix: "/simon",
                }
            },
            _ => {
                *res.status_mut() = StatusCode::Forbidden;
                return
            }
        };

        // build davhandler
        let fs = localfs::LocalFs::new(dirinfo.dirpath);
        let dav = DavHandler::new(dirinfo.prefix, fs);

        dav.handle(req, res);
    }
}

fn main() {
    env_logger::init().unwrap();

    let server = hyper::server::Server::http("0.0.0.0:4918").unwrap();
    server.handle_threads(Server::new(), 8).unwrap();
}

