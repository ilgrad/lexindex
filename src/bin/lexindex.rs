//! `lexindex`, the command line. The program is `src/cli.rs`, which the Python extension compiles
//! as well; this is the process around it.

#[path = "../cli.rs"]
mod cli;

fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = cli::run(
        &args,
        &mut std::io::stdin().lock(),
        &mut std::io::stdout().lock(),
        &mut std::io::stderr().lock(),
    );
    std::process::ExitCode::from(code)
}
