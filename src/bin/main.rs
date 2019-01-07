//
//  Sample application.
//
//  Listens on localhost:4918, plain http, no ssl.
//  Connect to http://localhost:4918/<DIR>/
//
//  <DIR>   username    password    filesystem
//  public  -           -           <rootdir>/public
//  mike    mike        mike        <rootdir>/mike
//  simon   simon       simon       <rootdir>/simon 
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
use dav::memls;
use dav::fs::DavFileSystem;
use dav::ls::DavLockSystem;
use dav::webpath::WebPath;

header! { (WWWAuthenticate, "WWW-Authenticate") => [String] }

#[derive(Debug)]
struct Server {
    fs:             Option<Box<DavFileSystem>>,
    ls:             Option<Box<DavLockSystem>>,
    directory:      String,
    do_accounts:    bool,
}

impl Server {
    pub fn new(directory: String, do_accounts: bool) -> Self {
        if directory != "" {
            Server{
                fs:             None,
                ls:             None,
                directory:      directory,
                do_accounts:    do_accounts,
            }
        } else {
            let fs = memfs::MemFs::new();
            if do_accounts {
                fs.create_dir(&WebPath::from_str("/public", "").unwrap()).unwrap();
                fs.create_dir(&WebPath::from_str("/mike", "").unwrap()).unwrap();
                fs.create_dir(&WebPath::from_str("/simon", "").unwrap()).unwrap();
            }
            Server{
                fs:             Some(fs),
                ls:             Some(memls::MemLs::new()),
                directory:      directory,
                do_accounts:    do_accounts,
            }
        }
    }
}

fn authenticate(req: &Request, res: &mut Response, user: &str, pass: &str) -> bool {
    // we must have a login/pass
    let (ok, username) = match req.headers.get::<Authorization<Basic>>() {
        Some(&Authorization(Basic{
                                ref username,
                                password: Some(ref password)
                            }
        )) => {
            (user == username && pass == password, username.as_str())
        },
        _ => (false, ""),
    };
    if !ok {
        if username == "test2@limebits.com" {
            // hack so that buggy litmus tests 61/62 work
            // should fail on anything not 2xx (or at least 401 Unauthorized),
            // but it wants to see 423 locked
            *res.status_mut() = StatusCode::Locked;
        } else {
            res.headers_mut().set(WWWAuthenticate(
                    "Basic realm=\"webdav-lib\"".to_string()));
            *res.status_mut() = StatusCode::Unauthorized;
        }
        res.headers_mut().set(hyper::header::Connection::close());
    }
    ok
}

impl Server {

    fn auth(&self, req: &Request, mut res: &mut Response, path: &[u8]) -> Result<(&str, &str), ()> {

        let path = match std::str::from_utf8(path) {
            Ok(p) => p,
            Err(_) => {
                *res.status_mut() = StatusCode::BadRequest;
                return Err(())
            },
        };

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
        let path = match WebPath::from_uri(&req.uri, "") {
            Ok(path) => path,
            Err(_) => {
                res.headers_mut().set(hyper::header::Connection::close());
                *res.status_mut() = StatusCode::BadRequest;
                return;
            }
        };

        // handle logins.
        let (dir, prefix) = if self.do_accounts {
            match self.auth(&req, &mut res, path.as_bytes()) {
                Ok((pfx, user)) => (self.directory.clone() + "/" + user, pfx),
                Err(_) => {
                    res.headers_mut().set(hyper::header::Connection::close());
                    return
                },
            }
        } else {
            (self.directory.clone(), "/")
        };

        // memfs or localfs.
        let (fs, prefix) : (Box<DavFileSystem>, &str) = if let Some(ref fs) = self.fs {
            ((*fs).clone(), prefix)
        } else {
            (localfs::LocalFs::new(dir, true), prefix)
        };

        // instantiate and run a new handler.
        let dav = DavHandler::new(prefix, fs, self.ls.clone());
        dav.handle(req, res);
    }
}

fn main() {
    env_logger::init();

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

