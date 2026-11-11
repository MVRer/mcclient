use mcclient::cli::{self, Parsed};
use mcclient::client::{self, Outcome};
use mcclient::text::Lang;
use std::process::exit;
use std::sync::Arc;

fn load_lang(path: Option<&std::path::Path>) -> Arc<Lang> {
    match Lang::load(path) {
        Ok(lang) => Arc::new(lang),
        Err(e) => {
            eprintln!("mcclient: {e}");
            exit(2);
        }
    }
}

fn main() {
    let options = match cli::parse(std::env::args().skip(1)) {
        Ok(Parsed::Run(o)) => o,
        Ok(Parsed::Mcp(defaults, lang)) => {
            let lang = load_lang(lang.as_deref());
            if let Err(e) = mcclient::mcp::serve(defaults, lang) {
                eprintln!("mcclient: {e}");
                exit(1);
            }
            exit(0);
        }
        Ok(Parsed::Help) => {
            print!("{}", cli::usage());
            exit(0);
        }
        Ok(Parsed::Version) => {
            println!("mcclient {}", env!("CARGO_PKG_VERSION"));
            exit(0);
        }
        Err(e) => {
            eprintln!("mcclient: {e}\n\n{}", cli::usage());
            exit(2);
        }
    };
    let lang = load_lang(options.lang.as_deref());
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    match client::run(&options, lang, &mut out) {
        Outcome::Completed => exit(0),
        Outcome::FailedBeforeJoin(msg) => {
            eprintln!("mcclient: {msg}");
            exit(1);
        }
        Outcome::Disconnected(msg) => {
            eprintln!("mcclient: {msg}");
            exit(3);
        }
    }
}
