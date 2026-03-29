mod autonomous;
mod autonomous_headless;
mod decomposition;
mod event_loop;
mod export;
mod helpers;
mod key_handling;
mod setup;
mod shell;
mod slash_autonomous;
mod slash_commands;
mod slash_share_corpus;
mod system_prompt;
mod turn_handling;

use anyhow::{bail, Result};
use shell::{
    run_ask, run_dashboard, run_health, run_ingest_corpus, run_login, run_recluster_corpus,
    run_shell,
};
use std::{env, path::PathBuf};

enum Command {
    Shell,
    Health,
    Login,
    Ask {
        prompt: String,
    },
    Run {
        problem: String,
        label: Option<String>,
        resume: Option<String>,
    },
    Dashboard {
        open: bool,
        port: Option<u16>,
    },
    ReclusterCorpus,
    IngestCorpus,
    Help,
}

struct CliOptions {
    command: Command,
    launch_cwd: PathBuf,
}

#[tokio::main]
async fn main() -> Result<()> {
    let options = parse_args(env::args().skip(1).collect::<Vec<_>>())?;
    match options.command {
        Command::Help => {
            print_help();
            Ok(())
        }
        Command::Health => run_health(options.launch_cwd).await,
        Command::Login => run_login().await,
        Command::Ask { prompt } => run_ask(prompt).await,
        Command::Run {
            problem,
            label,
            resume,
        } => autonomous_headless::run_autonomous(options.launch_cwd, problem, label, resume).await,
        Command::Dashboard { open, port } => run_dashboard(options.launch_cwd, open, port).await,
        Command::ReclusterCorpus => run_recluster_corpus().await,
        Command::IngestCorpus => run_ingest_corpus().await,
        Command::Shell => {
            if !setup::is_setup_complete() {
                match setup::run_wizard()? {
                    Some(result) => {
                        setup::save_config(&result)?;
                        eprintln!("Setup complete. Starting openproof...");
                    }
                    None => {
                        eprintln!("Setup cancelled.");
                        return Ok(());
                    }
                }
            }
            run_shell(options.launch_cwd).await
        }
    }
}

fn parse_args(args: Vec<String>) -> Result<CliOptions> {
    let launch_cwd = env::var("OPENPROOF_LAUNCH_CWD")
        .map(PathBuf::from)
        .unwrap_or_else(|_| env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

    if args.is_empty() {
        return Ok(CliOptions {
            command: Command::Shell,
            launch_cwd,
        });
    }

    if args
        .iter()
        .any(|arg| arg == "--help" || arg == "-h" || arg == "help")
    {
        return Ok(CliOptions {
            command: Command::Help,
            launch_cwd,
        });
    }

    if args.iter().any(|arg| arg == "--health")
        || args.first().map(String::as_str) == Some("health")
    {
        return Ok(CliOptions {
            command: Command::Health,
            launch_cwd,
        });
    }

    if args.iter().any(|arg| arg == "--login") || args.first().map(String::as_str) == Some("login")
    {
        return Ok(CliOptions {
            command: Command::Login,
            launch_cwd,
        });
    }

    if args.iter().any(|arg| arg == "--recluster-corpus")
        || args.first().map(String::as_str) == Some("recluster-corpus")
    {
        return Ok(CliOptions {
            command: Command::ReclusterCorpus,
            launch_cwd,
        });
    }

    if args.first().map(String::as_str) == Some("ingest") {
        return Ok(CliOptions {
            command: Command::IngestCorpus,
            launch_cwd,
        });
    }

    if args.first().map(String::as_str) == Some("dashboard") {
        let mut open = false;
        let mut port = None;
        let mut index = 1;
        while index < args.len() {
            match args[index].as_str() {
                "--open" => {
                    open = true;
                }
                "--port" => {
                    let Some(value) = args.get(index + 1) else {
                        bail!("dashboard --port requires a value");
                    };
                    port = Some(value.parse::<u16>()?);
                    index += 1;
                }
                unexpected => bail!("unknown dashboard argument: {unexpected}"),
            }
            index += 1;
        }
        return Ok(CliOptions {
            command: Command::Dashboard { open, port },
            launch_cwd,
        });
    }

    if args.first().map(String::as_str) == Some("ask") {
        let prompt = args.iter().skip(1).cloned().collect::<Vec<_>>().join(" ");
        if prompt.trim().is_empty() {
            bail!("openproof ask requires a prompt");
        }
        return Ok(CliOptions {
            command: Command::Ask { prompt },
            launch_cwd,
        });
    }

    if args.first().map(String::as_str) == Some("run") {
        let mut problem = String::new();
        let mut label = None;
        let mut resume = None;
        let mut index = 1;
        while index < args.len() {
            match args[index].as_str() {
                "--label" => {
                    index += 1;
                    label = args.get(index).cloned();
                }
                "--resume" => {
                    index += 1;
                    resume = args.get(index).cloned();
                }
                "--problem" => {
                    index += 1;
                    if let Some(p) = args.get(index) {
                        problem = p.clone();
                    }
                }
                other if problem.is_empty() && resume.is_none() => {
                    problem = other.to_string();
                }
                _ => {}
            }
            index += 1;
        }
        if problem.trim().is_empty() && resume.is_none() {
            bail!("openproof run requires a problem or --resume <session_id>. Usage: openproof run \"<problem>\" [--label <name>] [--resume <id>]");
        }
        return Ok(CliOptions {
            command: Command::Run {
                problem,
                label,
                resume,
            },
            launch_cwd,
        });
    }

    Ok(CliOptions {
        command: Command::Shell,
        launch_cwd,
    })
}

fn print_help() {
    println!(
        "\
openproof

Usage:
  openproof
  openproof health
  openproof login
  openproof ask <prompt>
  openproof run <problem> [--label <name>]
  openproof dashboard [--open] [--port <port>]
  openproof recluster-corpus

Legacy flags:
  --health    same as `openproof health`
  --login     same as `openproof login`
  --recluster-corpus same as `openproof recluster-corpus`
  --help      show this help"
    );
}
