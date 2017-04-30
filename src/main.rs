//!
//! rofld  -- Lulz on demand
//!

             extern crate ansi_term;
             extern crate atomic;
#[macro_use] extern crate clap;
             extern crate conv;
#[macro_use] extern crate custom_derive;
#[macro_use] extern crate error_derive;
             extern crate futures;
             extern crate futures_cpupool;
             extern crate glob;
             extern crate hyper;
             extern crate image;
             extern crate isatty;
             extern crate itertools;
#[macro_use] extern crate lazy_static;
             extern crate lru_cache;
#[macro_use] extern crate maplit;
#[macro_use] extern crate mime;
             extern crate nix;
             extern crate num;
             extern crate rusttype;
             extern crate serde;
#[macro_use] extern crate serde_json;
             extern crate serde_qs;
             extern crate slog_envlogger;
             extern crate slog_stdlog;
             extern crate slog_stream;
             extern crate tokio_core;
             extern crate tokio_signal;
             extern crate tokio_timer;
             extern crate time;
#[macro_use] extern crate try_opt;
             extern crate unreachable;

// `slog` must precede `log` in declarations here, because we want to simultaneously:
// * use the standard `log` macros (at least for a while)
// * be able to initialize the slog logger using slog macros like o!()
#[macro_use] extern crate slog;
#[macro_use] extern crate log;


#[cfg(test)]
#[macro_use] extern crate spectral;


#[macro_use]
mod util;

mod args;
mod ext;
mod caption;
mod logging;
mod model;
mod resources;
mod service;


use std::error::Error;
use std::env;
use std::io::{self, Write};
use std::process::exit;

use futures::{BoxFuture, Future, Stream, stream};
use hyper::server::{Http, NewService, Request, Response, Server};
use tokio_core::reactor::Handle;

use args::{ArgsError, Options};
use caption::CAPTIONER;


lazy_static! {
    /// Application / package name, as filled out by Cargo.
    static ref NAME: &'static str = option_env!("CARGO_PKG_NAME").unwrap_or("rofld");

    /// Application version, as filled out by Cargo.
    static ref VERSION: Option<&'static str> = option_env!("CARGO_PKG_VERSION");

    /// Application revision, such as Git SHA.
    /// This is generated by a build script and written to an output file.
    static ref REVISION: Option<&'static str> = {
        let revision = include_str!(concat!(env!("OUT_DIR"), "/", "revision"));
        if revision.trim().is_empty() { None } else { Some(revision) }
    };
}


fn main() {
    let opts = args::parse().unwrap_or_else(|e| {
        print_args_error(e).unwrap();
        exit(64);  // EX_USAGE
    });

    logging::init(opts.verbosity).unwrap();
    info!("{} {}{}", *NAME,
        VERSION.map(|v| format!("v{}", v)).unwrap_or_else(|| "<UNKNOWN VERSION>".into()),
        REVISION.map(|r| format!(" (rev. {})", r)).unwrap_or_else(|| "".into()));
    if let Some(pid) = get_process_id() {
        debug!("PID = {}", pid);
    }
    for (i, arg) in env::args().enumerate() {
        trace!("argv[{}] = {:?}", i, arg);
    }

    start_server(opts);
}

/// Print an error that may occur while parsing arguments.
fn print_args_error(e: ArgsError) -> io::Result<()> {
    match e {
        ArgsError::Parse(ref e) =>
            // In case of generic parse error,
            // message provided by the clap library will be the usage string.
            writeln!(&mut io::stderr(), "{}", e.message),
        e => {
            let mut msg = "Failed to parse arguments".to_owned();
            if let Some(cause) = e.cause() {
                msg += &format!(": {}", cause);
            }
            writeln!(&mut io::stderr(), "{}", msg)
        },
    }
}

#[cfg(unix)]
fn get_process_id() -> Option<i64> {
    Some(nix::unistd::getpid() as i64)
}

#[cfg(not(unix))]
fn get_process_id() -> Option<i64> {
    warn!("Cannot retrieve process ID on non-Unix platforms");
    None
}


/// Start the server with given options.
/// This function only terminates when the server finishes.
fn start_server(opts: Options) {
    info!("Starting the server to listen on {}...", opts.address);
    let mut server = Http::new().bind(&opts.address, || Ok(service::Rofl)).unwrap();

    set_config(opts, &mut server);
    let ctrl_c = create_ctrl_c_handler(&server.handle());

    debug!("Entering event loop...");
    server.run_until(ctrl_c).unwrap_or_else(|e| {
        error!("Failed to start the server's event loop: {}", e);
        exit(74);  // EX_IOERR
    });

    info!("Server stopped.");
}

/// Set configuration options from the command line flags.
fn set_config<S, B>(opts: Options, server: &mut Server<S, B>)
    where S: NewService<Request=Request,
                        Response=Response<B>,
                        Error=hyper::Error> + Send + Sync + 'static,
          B: Stream<Error=hyper::Error> + 'static, B::Item: AsRef<[u8]>
{
    trace!("Setting configuration options...");
    if let Some(rt_count) = opts.render_threads {
        CAPTIONER.set_thread_count(rt_count);
        debug!("Number of threads for image captioning set to {}", rt_count);
    }
    if let Some(tcs) = opts.template_cache_size {
        CAPTIONER.cache().set_template_capacity(tcs);
        debug!("Size of the template cache set to {}", tcs);
    }
    if let Some(fcs) = opts.font_cache_size {
        CAPTIONER.cache().set_font_capacity(fcs);
        debug!("Size of the font cache set to {}", fcs);
    }
    server.shutdown_timeout(opts.shutdown_timeout);
    debug!("Shutdown timeout set to {} secs", opts.shutdown_timeout.as_secs());
    CAPTIONER.set_task_timeout(opts.request_timeout);
    debug!("Request timeout set to {} secs", opts.request_timeout.as_secs());
}

/// Handle ^C and return a future a future that resolves when it's pressed.
fn create_ctrl_c_handler(handle: &Handle) -> BoxFuture<(), ()> {
    let max_ctrl_c_count = 3;
    trace!("Setting up ^C handler: once to shutdown gracefully, {} times to abort...",
        max_ctrl_c_count);

    tokio_signal::ctrl_c(handle)
        .flatten_stream()  // Future<Stream> -> Stream (with delayed first element)
        .map_err(|e| { error!("Error while handling ^C: {:?}", e); e })
        .zip(stream::iter((1..).into_iter().map(Ok)))
        .map(move |(x, i)| {
            match i {
                1 => info!("Received shutdown signal..."),
                i if i == max_ctrl_c_count => { info!("Aborted."); exit(0); },
                i => debug!("Got repeated ^C, {} more to abort", max_ctrl_c_count - i),
            };
            x
        })
        .into_future()  // Stream => Future<(first, rest)>
        .then(|_| Ok(()))
        .boxed()
}
