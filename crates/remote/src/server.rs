//! The studio's half: answer discovery, take ROMs, report progress, serve
//! the APK. The studio's window decides (Accept/Refuse) and builds; this
//! only keeps the list of requests and talks to phones.

use std::collections::BTreeMap;
use std::io;
use std::net::{SocketAddr, TcpListener, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::Duration;

use crate::http::{read_request, write_response};
use crate::{ANNOUNCE, DISCOVER, MAX_ROM, Progress, decode_query};

/// A phone's request, as the window shows it.
#[derive(Clone, Debug)]
pub struct Request {
    pub id: u32,
    /// The game's title, as the phone sent it.
    pub title: String,
    pub from: SocketAddr,
}

#[derive(Debug)]
struct Job {
    request: Request,
    rom: Vec<u8>,
    progress: Progress,
    apk: Option<Vec<u8>>,
}

#[derive(Debug, Default)]
struct Jobs {
    next: u32,
    jobs: BTreeMap<u32, Job>,
}

/// The studio's phone-build server; stops when dropped.
#[derive(Debug)]
pub struct Server {
    jobs: Arc<Mutex<Jobs>>,
    port: u16,
    stop: Arc<AtomicBool>,
    threads: Vec<JoinHandle<()>>,
}

fn lock(jobs: &Mutex<Jobs>) -> MutexGuard<'_, Jobs> {
    jobs.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl Server {
    /// Listen on TCP `port` (0: any free port) and answer discovery on
    /// UDP `discovery` (normally [`crate::PORT`]; `None`: don't), as `name`.
    pub fn start(name: &str, port: u16, discovery: Option<u16>) -> io::Result<Self> {
        let listener = TcpListener::bind(("0.0.0.0", port))?;
        listener.set_nonblocking(true)?;
        let port = listener.local_addr()?.port();
        let jobs = Arc::new(Mutex::new(Jobs::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let mut threads = Vec::new();

        if let Some(udp_port) = discovery {
            let socket = UdpSocket::bind(("0.0.0.0", udp_port))?;
            socket.set_read_timeout(Some(Duration::from_millis(200)))?;
            let (stop, reply) = (stop.clone(), format!("{ANNOUNCE} {port} {name}"));
            threads.push(std::thread::spawn(move || {
                let mut buf = [0u8; 64];
                while !stop.load(Ordering::Relaxed) {
                    if let Ok((n, from)) = socket.recv_from(&mut buf) {
                        if &buf[..n] == DISCOVER.as_bytes() {
                            let _ = socket.send_to(reply.as_bytes(), from);
                        }
                    }
                }
            }));
        }

        let (jobs2, stop2) = (jobs.clone(), stop.clone());
        threads.push(std::thread::spawn(move || {
            while !stop2.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut stream, from)) => {
                        let _ = stream.set_nonblocking(false);
                        let jobs = jobs2.clone();
                        std::thread::spawn(move || {
                            let _ = serve(&mut stream, from, &jobs);
                        });
                    }
                    Err(_) => std::thread::sleep(Duration::from_millis(50)),
                }
            }
        }));
        Ok(Self {
            jobs,
            port,
            stop,
            threads,
        })
    }

    #[must_use]
    pub fn port(&self) -> u16 {
        self.port
    }

    /// Requests nobody has answered yet.
    #[must_use]
    pub fn waiting(&self) -> Vec<Request> {
        lock(&self.jobs)
            .jobs
            .values()
            .filter(|j| j.progress == Progress::Waiting)
            .map(|j| j.request.clone())
            .collect()
    }

    /// The ROM of request `id` (to build it).
    #[must_use]
    pub fn rom(&self, id: u32) -> Option<Vec<u8>> {
        lock(&self.jobs).jobs.get(&id).map(|j| j.rom.clone())
    }

    /// Tell the phone where its build is.
    pub fn set_progress(&self, id: u32, progress: Progress) {
        if let Some(job) = lock(&self.jobs).jobs.get_mut(&id) {
            job.progress = progress;
        }
    }

    /// The build is done: the phone can download `apk`.
    pub fn finish(&self, id: u32, apk: Vec<u8>) {
        if let Some(job) = lock(&self.jobs).jobs.get_mut(&id) {
            job.progress = Progress::Done(apk.len());
            job.apk = Some(apk);
            job.rom = Vec::new();
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
    }
}

/// One connection: one request, one answer.
fn serve(stream: &mut std::net::TcpStream, from: SocketAddr, jobs: &Mutex<Jobs>) -> io::Result<()> {
    let req = match read_request(stream, MAX_ROM) {
        Ok(r) => r,
        Err(e) => return write_response(stream, 413, e.to_string().as_bytes()),
    };
    let (path, query) = req.target.split_once('?').unwrap_or((&req.target, ""));
    let parts: Vec<&str> = path.trim_matches('/').split('/').collect();
    match (req.method.as_str(), parts.as_slice()) {
        ("POST", ["builds"]) => {
            if req.body.is_empty() {
                return write_response(stream, 400, b"no ROM");
            }
            let title = query
                .split('&')
                .find_map(|kv| kv.strip_prefix("title="))
                .map_or_else(|| "game".to_string(), decode_query);
            let mut jobs = lock(jobs);
            jobs.next += 1;
            let id = jobs.next;
            jobs.jobs.insert(
                id,
                Job {
                    request: Request { id, title, from },
                    rom: req.body,
                    progress: Progress::Waiting,
                    apk: None,
                },
            );
            drop(jobs);
            write_response(stream, 201, id.to_string().as_bytes())
        }
        ("GET", ["builds", id]) => {
            let progress = id
                .parse()
                .ok()
                .and_then(|id: u32| lock(jobs).jobs.get(&id).map(|j| j.progress.clone()));
            match progress {
                Some(p) => write_response(stream, 200, p.encode().as_bytes()),
                None => write_response(stream, 404, b"no such build"),
            }
        }
        ("GET", ["builds", id, "apk"]) => {
            let apk = id
                .parse()
                .ok()
                .and_then(|id: u32| lock(jobs).jobs.get(&id).and_then(|j| j.apk.clone()));
            match apk {
                Some(apk) => write_response(stream, 200, &apk),
                None => write_response(stream, 409, b"not built (yet)"),
            }
        }
        _ => write_response(stream, 404, b"unknown"),
    }
}
