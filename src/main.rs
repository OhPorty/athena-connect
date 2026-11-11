//! athena-connect: gives an Athena server access to one folder on this
//! computer, and nothing else.

use athena_connect::{client, sandbox};

const HELP: &str = "athena-connect: give your Athena access to one folder on this computer, and nothing else.

  athena-connect login <address>   pair this computer with your Athena (once), e.g. https://athena.example.com
  athena-connect [folder]          give Athena access to this folder (default: the current one) until you close the terminal
  athena-connect logout            forget this computer's key
";

fn main() {
    let args: Vec<String> = std::env::args().collect();
    // The command helper: lock down, then become the shell. Never returns on success.
    if args.get(1).map(String::as_str) == Some("__run") {
        std::process::exit(run_helper(&args[2..]));
    }
    let result = match args.get(1).map(String::as_str) {
        Some("-h" | "--help" | "help") => {
            print!("{HELP}");
            Ok(())
        }
        Some("-V" | "--version") => {
            println!("athena-connect {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Some("login") => match args.get(2) {
            Some(server) => client::login(server),
            None => Err("Give your Athena's address: athena-connect login https://athena.example.com".into()),
        },
        Some("logout") => std::fs::remove_file(client::config_file()).map(|_| println!("This computer's key is forgotten. (Remove it in Athena's settings too.)")).map_err(|_| "This computer wasn't paired.".into()),
        Some(folder) => client::run(std::path::Path::new(folder)),
        None => client::run(&std::env::current_dir().unwrap_or_else(|_| ".".into())),
    };
    if let Err(e) = result {
        eprintln!("athena-connect: {e}");
        std::process::exit(1);
    }
}

/// `athena-connect __run <fence json> <command>`: run the command inside the sandbox.
fn run_helper(args: &[String]) -> i32 {
    let (Some(fence), Some(command)) = (args.first(), args.get(1)) else {
        eprintln!("athena-connect __run: missing arguments");
        return 2;
    };
    let fence: sandbox::Fence = match serde_json::from_str(fence) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("athena-connect __run: bad fence: {e}");
            return 2;
        }
    };
    sandbox::run(&fence, command)
}
