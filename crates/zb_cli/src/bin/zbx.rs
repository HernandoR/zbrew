use console::style;
use std::env;
use std::process::Command;

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();

    // Like a bare `zb`, a bare `zbx` is a request for usage, not a mistake:
    // it belongs on stdout and exits 0 so it can be piped and chained.
    if args.is_empty() {
        println!("zbx - Run a command from a formula without linking it");
        println!();
        println!("Usage: zbx <formula> [args...]");
        println!();
        println!("Examples:");
        println!("  zbx jq --version");
        println!("  zbx wget https://example.com");
        std::process::exit(0);
    }

    let zbx_path = env::current_exe().expect("failed to get current executable path");
    let zbx_dir = zbx_path
        .parent()
        .expect("failed to get parent directory of zbx");
    let zb_path = zbx_dir.join("zb");

    let mut cmd = Command::new(&zb_path);
    cmd.arg("run").args(&args);

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let err = cmd.exec();
        eprintln!("{} {}", style("error:").red().bold(), err);
        std::process::exit(1);
    }

    #[cfg(not(unix))]
    {
        match cmd.status() {
            Ok(status) => {
                std::process::exit(status.code().unwrap_or(1));
            }
            Err(e) => {
                eprintln!("{} {}", style("error:").red().bold(), e);
                std::process::exit(1);
            }
        }
    }
}
