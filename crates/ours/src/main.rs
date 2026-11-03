//! The Ours command line. The work lives in the library.

use ours::{config, run, schema_cmd};

use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(name = "ours", version = ours::VERSION, about = "The self-hosted AI agent for groups, companies, friends and families")]
struct Cli {
    /// Path to the configuration file.
    #[arg(short, long, default_value = "ours.toml")]
    config: PathBuf,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Run the agent: connectors, engine and admin page.
    Run,
    /// Check the configuration and print what would run.
    Check,
    /// Write the JSON Schemas of the engine's objects into a directory.
    Schema {
        #[arg(default_value = "schema")]
        out: PathBuf,
    },
    /// Ask the configured model one question with the engine's schema.
    ModelCheck,
    /// Re-run a group's journal through the engine and report what differs:
    /// with the logged answers (a determinism check) or with the configured
    /// model standing in for the logged replies.
    Replay {
        /// The group id from the configuration.
        group: String,
        /// Answer the engine's requests with the configured model instead of the log.
        #[arg(long)]
        live: bool,
        #[arg(long)]
        from: Option<u64>,
        #[arg(long)]
        to: Option<u64>,
        /// Write the Markdown report here instead of printing it.
        #[arg(long)]
        out: Option<PathBuf>,
        /// Print the report as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Show what the opt-in triage telemetry would send, from this host's own journals.
    Telemetry {
        /// The group id from the configuration; the first group when unset.
        #[arg(long)]
        group: Option<String>,
        /// How many records to print.
        #[arg(long, default_value_t = 5)]
        show: usize,
    },
    /// Score agents on the published scenarios: ours, wrapper, rules, or
    /// transcript:<path> for any other agent's recorded output.
    Bench {
        #[arg(long = "agent", required = true)]
        agents: Vec<ours::bench::AgentSpec>,
        #[arg(long, default_value = "benchmark/scenarios")]
        scenarios: PathBuf,
        /// Write the results page here instead of printing it.
        #[arg(long)]
        out: Option<PathBuf>,
        /// Save each agent's transcripts under this directory.
        #[arg(long)]
        transcripts: Option<PathBuf>,
    },
    /// List the starter personas a group can pick by name or by sentence.
    Personas,
    /// Write everything a group holds as JSON and Markdown, for a data request or a move.
    Export {
        group: String,
        #[arg(long, default_value = ".")]
        out: PathBuf,
    },
    /// Delete everything a group holds on this host. Stop `ours run` first.
    Wipe {
        group: String,
        /// Required: wipes are not undoable.
        #[arg(long)]
        yes: bool,
    },
    /// Print the Slack app manifest to paste into "Create app from manifest".
    Manifest,
    /// Print the Discord invite link for a bot application.
    Invite {
        /// The application's client id from the Discord developer portal.
        #[arg(long)]
        client_id: String,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();
    let cli = Cli::parse();
    match cli.command {
        Command::Check => {
            let config = config::Config::load(&cli.config)?;
            tracing::info!(
                data_dir = %config.host.data_dir.display(),
                admin_bind = %config.host.admin_bind,
                model = %config.model.model,
                base_url = %config.model.base_url,
                dialect = ?config.model.dialect,
                daily_cap_usd = config.model.daily_cap_usd,
                timeout_secs = config.model.timeout_secs,
                extra = %serde_json::Value::Object(config.model.extra.clone()),
                classifier = config.classifier.enabled,
                classifier_model = ?config.classifier.model,
                "host"
            );
            for connector in &config.connectors {
                tracing::info!(
                    platform = ?connector.platform,
                    agent_name = %connector.agent_name,
                    guilds = ?connector.guilds,
                    "connector"
                );
            }
            for group in &config.groups {
                tracing::info!(
                    id = %group.id,
                    name = %group.name,
                    kind = ?group.kind,
                    timezone = %group.timezone,
                    agent_name = group.agent_name.as_deref().unwrap_or("Q"),
                    rooms = group.rooms.len(),
                    dial = ?group.dial,
                    currency = %group.currency,
                    voice = ?group.voice,
                    leads = group.leads.len(),
                    hosts = group.hosts.len(),
                    "group"
                );
            }
            tracing::info!("configuration is valid");
            Ok(())
        }
        Command::Run => {
            let config = config::Config::load(&cli.config)?;
            run::run(config).await
        }
        Command::Schema { out } => {
            let count = schema_cmd::write(&out)?;
            tracing::info!(count, dir = %out.display(), "schemas written");
            Ok(())
        }
        Command::ModelCheck => {
            let config = config::Config::load(&cli.config)?;
            let plan = run::model_check(&config).await?;
            tracing::info!(message = ?plan.message, actions = ?plan.actions, "plan");
            Ok(())
        }
        Command::Replay {
            group,
            live,
            from,
            to,
            out,
            json,
        } => {
            let config = config::Config::load(&cli.config)?;
            let section = config
                .groups
                .iter()
                .find(|g| g.id == group)
                .ok_or_else(|| anyhow::anyhow!("no group {group:?} in the configuration"))?;
            let store = ours_store::StoreDir::open(&config.host.data_dir)?;
            let mut options =
                ours::replay::ReplayOptions::logged(ours_types::GroupId::new(group.clone()));
            options.kind = Some(section.kind);
            options.from_seq = from;
            options.to_seq = to;
            options.chat = ours::runtime::model::ChatSettings {
                temperature: config.model.chat_temperature,
                max_tokens: config.model.chat_max_tokens,
            };
            if live {
                let (model, dialect) = run::build_model(&config)?;
                let (classifier, extractor, resolver) =
                    run::build_small_models(&config, &model, dialect)?;
                options.strict_schema = dialect == ours_model::Dialect::OpenAi;
                options.oracle = ours::replay::Oracle::Live {
                    model,
                    classifier,
                    extractor,
                    resolver,
                };
            }
            let report = ours::replay::replay(&store, options).await?;
            let text = if json {
                serde_json::to_string_pretty(&report)?
            } else {
                ours::replay::render_markdown(&report)
            };
            match out {
                Some(path) => {
                    std::fs::write(&path, text)?;
                    tracing::info!(path = %path.display(), divergences = report.divergences.len(), "replay report written");
                }
                None => {
                    #[allow(clippy::print_stdout)]
                    {
                        println!("{text}");
                    }
                }
            }
            Ok(())
        }
        Command::Telemetry { group, show } => {
            let config = config::Config::load(&cli.config)?;
            let id = match group {
                Some(id) => id,
                None => config
                    .groups
                    .first()
                    .map(|g| g.id.clone())
                    .ok_or_else(|| anyhow::anyhow!("no groups configured"))?,
            };
            let store = ours_store::StoreDir::open(&config.host.data_dir)?;
            let (records, covered) =
                ours::telemetry::collect(&store, &ours_types::GroupId::new(id), 0)?;
            tracing::info!(
                records = records.len(),
                covered_seq = covered,
                enabled = config.telemetry.enabled,
                "telemetry sample; nothing is sent by this command"
            );
            #[allow(clippy::print_stdout)]
            {
                println!("{}", ours::telemetry::sample(&records, show));
            }
            Ok(())
        }
        Command::Bench {
            agents,
            scenarios,
            out,
            transcripts,
        } => {
            let options = ours::bench::BenchOptions {
                agents,
                scenarios,
                out: out.clone(),
                transcripts,
            };
            let live = if options.needs_model() {
                Some(ours::bench::OursStack::from_config(&config::Config::load(
                    &cli.config,
                )?)?)
            } else {
                None
            };
            let page = ours::bench::bench(&options, live.as_ref()).await?;
            match out {
                Some(path) => tracing::info!(path = %path.display(), "results written"),
                None => {
                    #[allow(clippy::print_stdout)]
                    {
                        println!("{page}");
                    }
                }
            }
            Ok(())
        }
        Command::Personas => {
            #[allow(clippy::print_stdout)]
            for starter in ours_core::starters() {
                let kinds: Vec<String> = starter
                    .kinds
                    .iter()
                    .map(|k| format!("{k:?}").to_lowercase())
                    .collect();
                println!(
                    "{:<8} dial {}  {}  ({})",
                    starter.name,
                    starter.dial,
                    starter.one_line,
                    kinds.join(", ")
                );
            }
            Ok(())
        }
        Command::Export { group, out } => {
            let config = config::Config::load(&cli.config)?;
            let section = config
                .groups
                .iter()
                .find(|g| g.id == group)
                .ok_or_else(|| anyhow::anyhow!("no group {group:?} in the configuration"))?;
            let id = ours_types::GroupId::new(group.clone());
            let store = ours_store::StoreDir::open(&config.host.data_dir)?;
            let journal: ours::runtime::journal::Journal<ours_core::Input, ours_core::GroupState> =
                ours::runtime::journal::Journal::new(store.open_group(&id)?);
            let recovered = journal.recover()?;
            let mut state = recovered
                .state
                .unwrap_or_else(|| ours_core::GroupState::new(id.clone(), section.kind));
            journal.replay(recovered.seq, |_, at, input| {
                let _ = ours_core::step(&mut state, input, at);
                Ok(())
            })?;
            let now = ours_types::Instant::from(jiff::Timestamp::now());
            let export = ours_core::render_export(&state, now);
            std::fs::create_dir_all(&out)?;
            let json_path = out.join(format!("{id}.export.json"));
            let md_path = out.join(format!("{id}.export.md"));
            std::fs::write(&json_path, serde_json::to_string_pretty(&export.json)?)?;
            std::fs::write(&md_path, export.markdown)?;
            tracing::info!(json = %json_path.display(), markdown = %md_path.display(), "exported");
            Ok(())
        }
        Command::Wipe { group, yes } => {
            if !yes {
                anyhow::bail!("wiping {group} cannot be undone; add --yes to confirm");
            }
            let config = config::Config::load(&cli.config)?;
            let id = ours_types::GroupId::new(group.clone());
            let store = ours_store::StoreDir::open(&config.host.data_dir)?;
            store.wipe_group(&id)?;
            for name in [format!("{id}.charter.md"), format!("{id}.decisions.md")] {
                let path = config.host.data_dir.join(name);
                if path.exists() {
                    std::fs::remove_file(path)?;
                }
            }
            tracing::warn!(%group, "wiped");
            Ok(())
        }
        Command::Manifest => {
            #[allow(clippy::print_stdout)]
            {
                println!("{}", ours_connector_slack::MANIFEST);
            }
            Ok(())
        }
        Command::Invite { client_id } => {
            tracing::info!(url = %ours_connector_discord::invite_url(&client_id), "invite");
            tracing::info!(
                "enable Server Members Intent and Message Content Intent under Bot in the developer portal"
            );
            Ok(())
        }
    }
}
