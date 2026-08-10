//! Entry point: pick a command from the registry and run it.

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();

    let Some(name) = args.first() else {
        usage();
        return ExitCode::from(1);
    };

    if name == "version" || name == "--version" || name == "-V" {
        println!("cml {}", env!("CARGO_PKG_VERSION"));
        return ExitCode::SUCCESS;
    }
    if name == "help" || name == "--help" || name == "-h" {
        usage();
        return ExitCode::SUCCESS;
    }

    let Some((run, _)) = cml::command(name) else {
        eprintln!("cml: unknown command '{name}'");
        usage();
        return ExitCode::from(1);
    };

    match run(&args[1..]) {
        Ok(code) => ExitCode::from(u8::try_from(code).unwrap_or(1)),
        Err(e) => {
            eprintln!("cml: {e}");
            ExitCode::from(1)
        }
    }
}

/// Generated from the command registry, so it cannot describe a command that no
/// longer exists or omit one that does.
fn usage() {
    println!("cml {} — full-history memory for Claude Code\n", env!("CARGO_PKG_VERSION"));
    println!("usage: cml <command> [args]\n");
    let width = cml::COMMANDS.iter().map(|c| c.len()).max().unwrap_or(8);
    for name in cml::COMMANDS {
        if let Some((_, help)) = cml::command(name) {
            println!("  {name:<width$}  {help}");
        }
    }
    println!("\n  search accepts --role <conversation|tools|scene>, --project P, --limit N,");
    println!("  --keyword / --semantic. With no --role it ranks every lane together.");
}
