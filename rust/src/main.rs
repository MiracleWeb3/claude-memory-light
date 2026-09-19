//! Entry point: pick a command from the registry and run it.

use std::process::ExitCode;

fn main() -> ExitCode {
    // `cml search x | head -3` closes the pipe after three lines, and Rust's
    // default SIGPIPE handling turns the next println into a panic with a
    // backtrace. Every command here is one a user pipes, and a memory tool that
    // panics into a `head` is a memory tool people stop piping.
    restore_sigpipe();

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

/// Make a closed stdout a quiet exit rather than a panic.
///
/// The usual fix is `signal(SIGPIPE, SIG_DFL)`, which needs `libc` and an
/// `unsafe` block — and this crate forbids `unsafe_code` at the manifest, on
/// purpose. A panic hook reaches the same observable behaviour in safe code:
/// the only panic a broken pipe can produce is the one `std` raises from its
/// own print machinery, and that one is recognisable by its message.
fn restore_sigpipe() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let msg = info
            .payload()
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| info.payload().downcast_ref::<&str>().copied())
            .unwrap_or_default();
        if msg.contains("Broken pipe") {
            // What a program killed by SIGPIPE looks like from the shell.
            std::process::exit(141);
        }
        default(info);
    }));
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
