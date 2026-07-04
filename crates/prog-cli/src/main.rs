use std::{path::PathBuf, process::ExitCode};

use clap::{Args, Parser, Subcommand, ValueEnum, error::ErrorKind};
use prog_core::{CoreError, PreviewPolicy, Result, SliceRequest, Store, expand};
use serde::Serialize;
use tracing_subscriber::{EnvFilter, fmt::writer::MakeWriterExt};

#[derive(Debug, Parser)]
#[command(
    name = "prog",
    version,
    about = "Progressive-disclosure gateway for APIs, CLIs, and MCP servers"
)]
struct Cli {
    #[arg(long, env = "PROG_DIR", default_value = "./.prog", global = true)]
    dir: PathBuf,

    #[arg(long, global = true)]
    pretty: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    Discover(DiscoverArgs),
    Hints(HintsArgs),
    Call(CallArgs),
    Expand(ExpandArgs),
    Cache {
        #[command(subcommand)]
        command: CacheCommand,
    },
    Meta(MetaArgs),
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum SourceKind {
    Http,
    Cli,
    Mcp,
}

#[derive(Debug, Args)]
struct DiscoverArgs {
    source_id: String,

    #[arg(long)]
    kind: SourceKind,

    #[arg(long)]
    seed: String,

    #[arg(long)]
    probe: bool,
}

#[derive(Debug, Args)]
struct HintsArgs {
    source_id: String,
    operation: Option<String>,
}

#[derive(Debug, Args)]
struct CallArgs {
    source_id: String,
    operation: String,

    #[arg(long)]
    args: String,

    #[arg(long)]
    view: Option<String>,

    #[arg(long)]
    yes: bool,

    #[arg(long)]
    no_cache: bool,

    #[arg(long)]
    refresh: bool,
}

#[derive(Debug, Args)]
struct ExpandArgs {
    cursor: String,

    #[arg(long, default_value = "")]
    path: String,

    #[arg(long)]
    limit: Option<usize>,

    #[arg(long)]
    depth: Option<usize>,

    #[arg(long, value_delimiter = ',')]
    fields: Vec<String>,

    #[arg(long)]
    out: Option<PathBuf>,
}

#[derive(Debug, Subcommand)]
enum CacheCommand {
    List,
    Get(CacheGetArgs),
    Purge(CachePurgeArgs),
}

#[derive(Debug, Args)]
struct CacheGetArgs {
    key: String,
}

#[derive(Debug, Args)]
struct CachePurgeArgs {
    #[arg(long)]
    source: Option<String>,

    #[arg(long)]
    expired: bool,

    #[arg(long)]
    all: bool,
}

#[derive(Debug, Args)]
struct MetaArgs {
    contract: Option<String>,
}

#[tokio::main]
async fn main() -> ExitCode {
    init_tracing();

    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(err)
            if matches!(
                err.kind(),
                ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
            ) =>
        {
            let _ = err.print();
            return ExitCode::from(err.exit_code() as u8);
        }
        Err(err) => {
            let error = CoreError::CliUsage(err.to_string());
            return write_error(&error, false);
        }
    };

    match run(&cli).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => write_error(&error, cli.pretty),
    }
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("off"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr.with_max_level(tracing::Level::TRACE))
        .try_init();
}

async fn run(cli: &Cli) -> Result<()> {
    let _store_dir = &cli.dir;

    match &cli.command {
        Command::Discover(args) => {
            let _ = (&args.source_id, args.kind, &args.seed, args.probe);
            not_implemented("discover")
        }
        Command::Hints(args) => {
            let _ = (&args.source_id, &args.operation);
            not_implemented("hints")
        }
        Command::Call(args) => {
            let _ = (
                &args.source_id,
                &args.operation,
                &args.args,
                &args.view,
                args.yes,
                args.no_cache,
                args.refresh,
            );
            not_implemented("call")
        }
        Command::Expand(args) => {
            let _ = (
                &args.cursor,
                &args.path,
                &args.limit,
                &args.depth,
                &args.fields,
                &args.out,
            );
            not_implemented("expand")
        }
        Command::Cache { command } => match command {
            CacheCommand::List => {
                let store = Store::open(&cli.dir)?;
                write_success(&store.list_entries(100)?, cli.pretty)
            }
            CacheCommand::Get(args) => {
                let store = Store::open(&cli.dir)?;
                let entry = store
                    .get_entry(&args.key)?
                    .ok_or_else(|| CoreError::CacheMiss(args.key.clone()))?;
                let payload = store
                    .get_payload(&entry.payload_hash)?
                    .ok_or_else(|| CoreError::CacheMiss(args.key.clone()))?;
                let projection = expand(
                    &payload,
                    "",
                    &SliceRequest {
                        path: None,
                        limit: None,
                        depth: None,
                        fields: Vec::new(),
                        omit: Vec::new(),
                        extra: serde_json::Map::new(),
                    },
                    &PreviewPolicy::default(),
                )?;
                write_success(&CacheGetOutput { entry, projection }, cli.pretty)
            }
            CacheCommand::Purge(args) => {
                let store = Store::open(&cli.dir)?;
                let summary = if args.all {
                    store.purge_all()?
                } else if args.expired {
                    store.purge_expired(chrono::Utc::now())?
                } else if let Some(source) = &args.source {
                    store.purge_source(source)?
                } else {
                    return Err(CoreError::BadArgs {
                        operation: "cache purge".to_string(),
                        reason: "pass --all, --expired, or --source <id>".to_string(),
                    });
                };
                write_success(&summary, cli.pretty)
            }
        },
        Command::Meta(args) => {
            let _ = &args.contract;
            not_implemented("meta")
        }
    }
}

#[derive(Serialize)]
struct CacheGetOutput {
    entry: prog_core::CacheEntryMeta,
    projection: prog_core::Projection,
}

fn not_implemented(command: &'static str) -> Result<()> {
    Err(CoreError::NotImplemented { command })
}

fn write_success<T: Serialize>(value: &T, pretty: bool) -> Result<()> {
    if pretty {
        println!("{}", serde_json::to_string_pretty(value)?);
    } else {
        println!("{}", serde_json::to_string(value)?);
    }
    Ok(())
}

fn write_error(error: &CoreError, pretty: bool) -> ExitCode {
    let envelope = error.envelope();
    let rendered = if pretty {
        serde_json::to_string_pretty(&envelope)
    } else {
        serde_json::to_string(&envelope)
    };

    match rendered {
        Ok(json) => {
            println!("{json}");
            ExitCode::FAILURE
        }
        Err(json_error) => {
            let fallback = CoreError::Json(json_error);
            println!(
                "{}",
                serde_json::to_string(&fallback.envelope()).unwrap_or_else(|_| {
                    "{\"error\":{\"kind\":\"json\",\"message\":\"failed to render error\",\"hint\":\"Report this bug.\"}}".to_string()
                })
            );
            ExitCode::FAILURE
        }
    }
}
