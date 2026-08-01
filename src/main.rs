use clap::Parser;
use numan_cli::cli::{Cli, Commands};
use numan_cli::config;
use numan_cli::core;

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    let platform = core::platform::Platform::detect();
    let root = cli
        .root
        .unwrap_or_else(|| config::Config::resolve_root(&platform));

    // Ensure root directory exists (completions/doctor/setup/use do not need layout)
    if !matches!(
        cli.command,
        Commands::Completions(_) | Commands::Doctor(_) | Commands::Setup(_) | Commands::Use(_)
    ) {
        std::fs::create_dir_all(&root)?;
    }

    match cli.command {
        Commands::Search(args) => numan_cli::cmd::search::execute(&args, &root),
        Commands::Info { id } => numan_cli::cmd::info::execute(&id, &root),
        Commands::Install(args) => numan_cli::cmd::install::execute(&args, &root),
        Commands::Update(args) => numan_cli::cmd::update::execute(&args, &root),
        Commands::Remove(args) => numan_cli::cmd::remove::execute(&args, &root),
        Commands::Gc(args) => numan_cli::cmd::gc::execute(&args, &root),
        Commands::Activate(args) => numan_cli::cmd::activate::execute(&args, &root),
        Commands::Deactivate(args) => numan_cli::cmd::deactivate::execute(&args, &root),
        Commands::List => numan_cli::cmd::list::execute(&root),
        Commands::Init(args) => numan_cli::cmd::init::execute(&args, &root),
        Commands::Registry(cmd) => numan_cli::cmd::registry::execute(cmd, &root),
        Commands::Snapshot(cmd) => numan_cli::cmd::snapshot::execute(cmd, &root),
        Commands::Nupm(args) => {
            let mut stdout = std::io::stdout();
            numan_cli::cmd::nupm::execute(&args, &root, &mut stdout)
        }
        Commands::Completions(args) => numan_cli::cmd::completions::execute(&args),
        Commands::Doctor(args) => {
            let code = numan_cli::cmd::doctor::execute(&args, &root)?;
            std::process::exit(code);
        }
        Commands::Setup(cmd) => numan_cli::cmd::setup::execute(cmd, &root),
        Commands::Try(args) => numan_cli::cmd::try_cmd::execute(&args, &root),
        Commands::Use(args) => numan_cli::cmd::use_cmd::execute(&args, &root),
    }
}
