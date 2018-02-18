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

#[macro_use] extern crate hyper;
#[macro_use] extern crate clap;
extern crate log;
extern crate env_logger;
extern crate webdav_handler;

use hyper::header::{Authorization, Basic};
use hyper::server::{Handler,Request, Response};
use hyper::status::StatusCode;

use webdav_handler as dav;
use dav::DavHandler;
use dav::localfs;
use dav::memfs;
use dav::fs::DavFileSystem;

header! { (WWWAuthenticate, "WWW-Authenticate") => [String] }

#[derive(Debug)]
struct Server {
    fs:             Option<Box<DavFileSystem>>,
    directory:      String,
    do_accounts:    bool,
}

impl Server {
    pub fn new(directory: String, do_accounts: bool) -> Self {
        let fs : Option<Box<DavFileSystem>> = if directory != "" {
            None
        } else {
            Some(memfs::MemFs::new())
        };
        Server{
            fs:             fs,
            directory:      directory,
            do_accounts:    do_accounts,
        }
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

impl Server {

    fn auth(&self, req: &Request, mut res: &mut Response, path: String) -> Result<(&str, &str), ()> {

        // path can start with "/public" (no authentication needed)
        // or "/username". known users are "mike" and "simon".
        //
        // NOTE we no not percent-decode here, if this was a real
        // application that would be bad.
        let x = path.splitn(3, "/").collect::<Vec<&str>>();
        let (prefix, dir) = match x[1] {
            "public" => ("/public", "public" ),
            "mike" => {
                if !authenticate(&req, &mut res, "mike", "mike") {
                    return Err(());
                }
                ("/mike", "mike")
            },
            "simon" => {
                if !authenticate(&req, &mut res, "simon", "simon") {
                    return Err(());
                }
                ("/simon", "simon")
            },
            _ => {
                *res.status_mut() = StatusCode::Forbidden;
                return Err(());
            }
        };
        Ok((prefix, dir))
    }
}

impl Handler for Server {

    //fn handle<'a, 'k>(&'a self, req: Request<'a, 'k>, mut res: Response<'a>) {
    fn handle(&self, req: Request, mut res: Response) {

        // Get request path.
        let path = match req.uri {
            hyper::uri::RequestUri::AbsolutePath(ref s) => s.to_string(),
            _ => {
                *res.status_mut() = StatusCode::BadRequest;
                return;
            }
        };

        // handle logins.
        let (dir, prefix) = if self.do_accounts {
            match self.auth(&req, &mut res, path) {
                Ok((d, p)) => (self.directory.clone() + "/" + d, p),
                Err(_) => return,
            }
        } else {
            (self.directory.clone(), "/")
        };

        // memfs or localfs.
        let (fs, prefix) : (Box<DavFileSystem>, &str) = if let Some(ref fs) = self.fs {
            ((*fs).clone(), "/")
        } else {
            (localfs::LocalFs::new(dir, true), prefix)
        };

        // instantiate and run a new handler.
        let dav = DavHandler::new(prefix, fs);
        dav.handle(req, res);
    }
}

fn main() {
    env_logger::init().unwrap();

    let matches = clap_app!(webdav_lib =>
        (version: "0.1")
        (@arg PORT: -p --port +takes_value "port to listen on (4918)")
        (@arg DIR: -d --dir +takes_value "local directory to serve")
        (@arg mem: -m --memfs "serve from ephemeral memory filesystem")
        (@arg accounts: -a --accounts "with fake mike/simon accounts")
    ).get_matches();

    let (dir, name) = match matches.value_of("DIR") {
        Some(dir) => (dir, dir),
        None => ("", "memory filesystem"),
    };

    let port = matches.value_of("PORT").unwrap_or("4918");
    let port = "0.0.0.0:".to_string() + port;
    let hyper_server = hyper::server::Server::http(&port).unwrap();
    let dav_server = Server::new(dir.to_string(), matches.is_present("accounts"));

    println!("Serving {} on {}", name, port);
    hyper_server.handle_threads(dav_server, 8).unwrap();
}

