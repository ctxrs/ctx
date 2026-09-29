use std::{
    io::{BufRead, BufReader, Read, Write},
    net::{TcpListener, TcpStream},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use crate::Endpoint;

#[derive(Clone)]
pub(super) struct Request {
    pub method: String,
    pub path: String,
    pub authorization: String,
    pub body: Vec<u8>,
}

pub(super) enum Response {
    Drop,
    Raw(u16, String, Vec<(String, String)>),
}
impl Response {
    pub fn json(status: u16, value: &impl serde::Serialize) -> Self {
        Self::Raw(status, serde_json::to_string(value).unwrap(), vec![])
    }
}

pub(super) struct Mock {
    endpoint: Endpoint,
    requests: Arc<Mutex<Vec<Request>>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Mock {
    pub fn new(mut respond: impl FnMut(&Request) -> Response + Send + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let endpoint =
            Endpoint::parse(&format!("http://{}", listener.local_addr().unwrap())).unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = requests.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = stop.clone();
        let thread = thread::spawn(move || {
            while !stopping.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream
                            .set_read_timeout(Some(Duration::from_secs(2)))
                            .unwrap();
                        let request = read_request(&stream);
                        captured.lock().unwrap().push(request.clone());
                        if let Response::Raw(status, body, headers) = respond(&request) {
                            write!(stream, "HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n",body.len()).unwrap();
                            for (key, value) in headers {
                                write!(stream, "{key}: {value}\r\n").unwrap();
                            }
                            write!(stream, "\r\n{body}").unwrap();
                            stream.flush().unwrap();
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(2))
                    }
                    Err(e) => panic!("local mock failed: {e}"),
                }
            }
        });
        Self {
            endpoint,
            requests,
            stop,
            thread: Some(thread),
        }
    }

    pub fn endpoint(&self) -> Endpoint {
        self.endpoint.clone()
    }
    pub fn requests(&self) -> Vec<Request> {
        self.requests.lock().unwrap().clone()
    }
}

impl Drop for Mock {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            if !std::thread::panicking() {
                thread.join().unwrap();
            }
        }
    }
}

fn read_request(stream: &TcpStream) -> Request {
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    let first = line.split_whitespace().collect::<Vec<_>>();
    let (method, path) = (first[0].to_owned(), first[1].to_owned());
    let mut length = 0;
    let mut authorization = String::new();
    loop {
        line.clear();
        reader.read_line(&mut line).unwrap();
        if line == "\r\n" {
            break;
        }
        let (key, value) = line.trim_end().split_once(':').unwrap();
        if key.eq_ignore_ascii_case("content-length") {
            length = value.trim().parse().unwrap();
        }
        if key.eq_ignore_ascii_case("authorization") {
            authorization = value.trim().to_owned();
        }
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body).unwrap();
    Request {
        method,
        path,
        authorization,
        body,
    }
}
