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
#[macro_use] extern crate lazy_static;
             extern crate lru_cache;
#[macro_use] extern crate maplit;
#[macro_use] extern crate mime;
             extern crate num;
             extern crate rusttype;
             extern crate serde;
#[macro_use] extern crate serde_derive;
#[macro_use] extern crate serde_json;
             extern crate serde_qs;
             extern crate slog_envlogger;
             extern crate slog_stdlog;
             extern crate slog_stream;
             extern crate tokio_signal;
             extern crate tokio_timer;
             extern crate time;
#[macro_use] extern crate try_opt;

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
mod service;


use std::error::Error;
use std::env;
use std::io::{self, Write};
use std::process::exit;

use futures::{Future, Stream};
use hyper::server::Http;

use args::{ArgsError, Options};
use caption::CAPTIONER;


lazy_static! {
    /// Application / package name, as filled out by Cargo.
    static ref NAME: &'static str = option_env!("CARGO_PKG_NAME").unwrap_or("rofld");

    /// Application version, as filled out by Cargo.
    static ref VERSION: Option<&'static str> = option_env!("CARGO_PKG_VERSION");

    /// Application revision, such as Git SHA.
    /// This is generated by a build script and written to an output file.
    static ref REVISION: Option<&'static str> = Some(
        include_str!(concat!(env!("OUT_DIR"), "/", "revision")));
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

/// Start the server with given options.
/// This function only terminated when the server finishes.
fn start_server(opts: Options) {
    info!("Starting the server to listen on {}...", opts.address);
    let mut server = Http::new().bind(&opts.address, || Ok(service::Rofl)).unwrap();

    server.shutdown_timeout(opts.shutdown_timeout);
    trace!("Shutdown timeout set to {} secs", opts.shutdown_timeout.as_secs());
    CAPTIONER.set_task_timeout(opts.request_timeout);
    trace!("Request timeout set to {} secs", opts.request_timeout.as_secs());

    trace!("Setting up ^C handler...");
    let ctrl_c = tokio_signal::ctrl_c(&server.handle())
        .flatten_stream().into_future()  // Future<Stream> => Future<(first, rest)>
        .map(|x| { info!("Received shutdown signal..."); x })
        .then(|_| Ok(()));

    debug!("Entering event loop...");
    server.run_until(ctrl_c).unwrap_or_else(|e| {
        error!("Failed to start the server's event loop: {}", e);
        exit(74);  // EX_IOERR
    });

    info!("Server stopped.");
}
