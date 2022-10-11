#[macro_use]
extern crate slog;
extern crate clap;
extern crate slog_async;
extern crate slog_term;

use clap::{App, Arg};
use slog::{Drain, Level};
use std::io::{self, BufRead, BufReader, BufWriter, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

static TOTAL_STREAMS: AtomicUsize = AtomicUsize::new(0);

fn handle_client(
    log: slog::Logger,
    mut reader: BufReader<TcpStream>,
    mut writer: BufWriter<TcpStream>,
) -> io::Result<thread::JoinHandle<()>> {
    let builder = thread::Builder::new();
    builder.spawn(move || {
        let mut buf = String::with_capacity(2048);

        while let Ok(sz) = reader.read_line(&mut buf) {
            info!(log, "Received a {} bytes: {}", sz, buf);
            if sz == 0 {
                warn!(log, "Received a {} bytes: exiting", sz);
                break;
            }
            writer
                .write_all(buf.as_bytes())
                .expect("could not write line");
            writer.flush().unwrap();
            buf.clear();
        }
        crit!(log, "terminate");
        TOTAL_STREAMS.fetch_sub(1, Ordering::Relaxed);
    })
}

fn cleaner_thread(
    log: slog::Logger,
    vec: Arc<Mutex<Vec<JoinHandle<()>>>>,
) -> JoinHandle<()> {
    let builder = thread::Builder::new();
    builder
        .spawn(move || loop {
            let moved_out = {
                let (mut moved_out, mut returned) = (vec![], vec![]);

                let mut guard = vec.lock().unwrap();
                crit!(log, "threads {}", guard.len());
                {
                    let drain = guard.drain(..);
                    for el in drain {
                        if el.is_finished() {
                            moved_out.push(el);
                        } else {
                            returned.push(el);
                        }
                    }
                }
                for el in returned {
                    guard.push(el);
                }
                moved_out
            };

            for el in moved_out {
                el.join().unwrap();
            }
            thread::sleep(Duration::new(10, 0));
        })
        .unwrap()
}

fn main() {
    let decorator = slog_term::TermDecorator::new().build();
    let drain = slog_term::CompactFormat::new(decorator).build().fuse();
    let drain = slog::LevelFilter::new(drain, Level::Critical).fuse();
    let drain = slog_async::Async::new(drain).build().fuse();
    let root = slog::Logger::root(drain, o!());

    let matches = App::new("server")
        .arg(
            Arg::with_name("host")
                .long("host")
                .value_name("HOST")
                .help("Sets which hostname to listen on")
                .takes_value(true),
        )
        .arg(
            Arg::with_name("port")
                .long("port")
                .value_name("PORT")
                .help("Sets which port to listen on")
                .takes_value(true),
        )
        .get_matches();

    let host: &str = matches.value_of("host").unwrap_or("localhost");
    let port = matches
        .value_of("port")
        .unwrap_or("1987")
        .parse::<u16>()
        .expect("port-no not valid");

    let listener = TcpListener::bind((host, port)).unwrap();
    let server = root.new(o!("host" => host.to_string(), "port" => port));
    info!(server, "Server open for business! :D");

    let joins = Arc::new(Mutex::new(Vec::new()));

    let cleaner_log = root.new(o!("cleaner" => "thread"));
    let _par_thread = cleaner_thread(cleaner_log, joins.clone());
    for stream in listener.incoming() {
        if let Ok(stream) = stream {
            let stream_no = TOTAL_STREAMS.fetch_add(1, Ordering::Relaxed);
            let log = root.new(o!("stream-no" => stream_no,
                   "peer-addr" => stream.peer_addr().expect("no peer address").to_string()));
            let writer =
                BufWriter::new(stream.try_clone().expect("could not clone stream"));
            let reader = BufReader::new(stream);
            match handle_client(log, reader, writer) {
                Ok(handler) => {
                    joins.lock().unwrap().push(handler);
                }
                Err(err) => {
                    error!(server, "Could not make client handler. {:?}", err);
                }
            }
        } else {
            slog_crit!(root, "Shutting down! {:?}", stream);
        }
    }

    info!(
        server,
        "No more incoming connections. Draining existing connections."
    );
}
