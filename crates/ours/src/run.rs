//! Wires the configuration into a running process: connectors, one actor
//! per group, the model, the timers and the admin page.

use crate::config::{Config, ModelDialect};
use crate::runtime::admin::AdminHub;
use crate::runtime::group::{Desired, GroupRunner, RunEnd, WipeRequest, Wiring};
use crate::runtime::journal::Journal;
use crate::runtime::router::Router;
use ours_admin::{AdminConfig, AdminServer, AdminSource, qr_lines_for};
use ours_connector::Connector;
use ours_connector_discord::{DiscordConfig, DiscordConnector};
use ours_connector_slack::{SlackConfig, SlackConnector};
use ours_connector_telegram::{TelegramConfig, TelegramConnector};
use ours_core::{GroupState, Input};
use ours_mail::{Inbox, Mailer};
use ours_model::{
    Classifier, Dialect, Extractor, LargeModel, ModelConfig, NoExtractor, NoTimeResolver,
    OpenAiCompatible, RulesOnlyClassifier, SmallModelClassifier, SmallModelExtractor,
    SmallModelTimeResolver, SpendMeter, TimeResolver,
};
use ours_store::StoreDir;
use ours_types::{GroupId, InboundEvent, Outbound, Platform};
use std::collections::HashMap;
use std::net::{IpAddr, UdpSocket};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::{mpsc, watch};

pub async fn run(config: Config) -> anyhow::Result<()> {
    let store = Arc::new(StoreDir::open(&config.host.data_dir)?);
    let hub = AdminHub::new();

    let (model, dialect) = build_model(&config)?;
    let (classifier, extractor, resolver) = build_small_models(&config, &model, dialect)?;
    let (mailer, inbox) = build_mail(&config)?;
    hub.set_catalogue(
        ours_core::catalogue()
            .iter()
            .filter_map(|e| serde_json::to_value(e).ok())
            .map(|mut v| {
                let tier = match v.get("tier").and_then(|t| t.as_str()) {
                    Some("free") => 0,
                    Some("windowed") => 1,
                    _ => 2,
                };
                v["tier"] = serde_json::Value::from(tier);
                v
            })
            .collect(),
    );

    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    // Set when a connector gives up, so the exit code tells a supervisor to restart.
    let connector_died = Arc::new(std::sync::atomic::AtomicBool::new(false));

    // Where members reach the admin server for calendar feeds: the host's
    // public URL when set, otherwise the LAN address the page prints.
    let public_base = config.host.public_url.clone().unwrap_or_else(|| {
        let ip = lan_ip().unwrap_or(IpAddr::V4(std::net::Ipv4Addr::LOCALHOST));
        format!("http://{ip}:{}", config.host.admin_bind.port())
    });

    // Connectors: one task each, one outbound queue each, one inbound queue each.
    // A connector that gives up stops the whole process, loudly, rather than
    // leaving an agent that can never hear anything.
    let mut outbound: HashMap<Platform, mpsc::Sender<Outbound>> = HashMap::new();
    let mut inbound: Vec<(Platform, mpsc::Receiver<InboundEvent>)> = Vec::new();
    let mut tasks = Vec::new();
    for section in &config.connectors {
        let connector: Box<dyn Connector> = match section.platform {
            Platform::Discord => Box::new(DiscordConnector::new(DiscordConfig {
                token: section.token.clone().unwrap_or_default(),
                agent_name: section.agent_name.clone(),
                guilds: section.guilds.clone(),
            })?),
            Platform::Slack => Box::new(SlackConnector::new(SlackConfig {
                bot_token: section.token.clone().unwrap_or_default(),
                app_token: section.app_token.clone().unwrap_or_default(),
                agent_name: section.agent_name.clone(),
                leads_usergroup: section.leads_usergroup.clone(),
            })?),
            Platform::Telegram => {
                // Telegram cannot list a bot's chats, so the configured rooms
                // are synced at startup instead of at their next message.
                let chats: Vec<String> = config
                    .groups
                    .iter()
                    .flat_map(|g| g.rooms.iter())
                    .filter(|r| r.platform == Platform::Telegram)
                    .map(|r| r.id.clone())
                    .collect();
                Box::new(
                    TelegramConnector::new(TelegramConfig {
                        token: section.token.clone().unwrap_or_default(),
                        agent_name: section.agent_name.clone(),
                    })?
                    .with_chats(chats),
                )
            }
            other => anyhow::bail!("the {other:?} connector is not built yet"),
        };
        let (out_tx, out_rx) = mpsc::channel(256);
        let (in_tx, in_rx) = mpsc::channel(1024);
        let platform = section.platform;
        let trip = shutdown_tx.clone();
        let died = Arc::clone(&connector_died);
        tasks.push(tokio::spawn(async move {
            match connector.run(in_tx, out_rx).await {
                Ok(()) => tracing::warn!(?platform, "connector finished"),
                Err(error) => tracing::error!(?platform, %error, "connector stopped"),
            }
            died.store(true, std::sync::atomic::Ordering::SeqCst);
            let _ = trip.send(true);
        }));
        outbound.insert(platform, out_tx);
        inbound.push((platform, in_rx));
    }

    // Groups: queues first, so the router can be shared before the actors exist.
    let mut router = Router::new();
    let mut pending = Vec::new();
    let mut group_queues: Vec<mpsc::Sender<Input>> = Vec::new();
    for section in &config.groups {
        let id = GroupId::new(section.id.clone());
        let rooms: Vec<(Platform, ours_types::PlatformRoomId)> = section
            .rooms
            .iter()
            .map(|r| (r.platform, r.id.as_str().into()))
            .collect();
        let (tx, rx) = mpsc::channel(1024);
        router.register(id.clone(), &rooms, tx.clone());
        group_queues.push(tx.clone());
        pending.push((id, section, rooms, tx, rx));
    }
    let router = Arc::new(router);

    for (id, section, rooms, tx, rx) in pending {
        let journal = Journal::new(store.open_group(&id)?);
        let feed_secret = journal.feed_secret()?;
        hub.set_feed_secret(&id, feed_secret.clone());
        let feed_base = format!(
            "{}/ics/{id}/{feed_secret}",
            public_base.trim_end_matches('/')
        );
        let desired = Desired {
            name: section.name.clone(),
            kind: section.kind,
            agent_name: section.agent_name.clone(),
            timezone: section.timezone.clone(),
            leads: section.leads.clone(),
            hosts: section.hosts.clone(),
            rooms,
            model_label: format!(
                "{}, running locally on the host's machine",
                config.model.model
            ),
            dial: section.dial,
            voice: section.voice.clone(),
            currency: section.currency.clone(),
            traits: section.traits.clone(),
            bits: section.bits.clone(),
            never_jokes_about: section.never_jokes_about.clone(),
            persona: section.persona.clone(),
            legal_hold: section.legal_hold,
            feed_base: Some(feed_base),
        };
        let (wipe_tx, wipe_rx) = mpsc::channel(1);
        hub.set_controls(&id, tx.clone(), wipe_tx);
        let wiring_parts = Arc::new(WiringParts {
            self_tx: tx.clone(),
            outbound: outbound.clone(),
            model: Arc::clone(&model),
            classifier: Arc::clone(&classifier),
            extractor: Arc::clone(&extractor),
            resolver: Arc::clone(&resolver),
            mailer: mailer.clone(),
            strict_schema: dialect == Dialect::OpenAi,
            chat: crate::runtime::model::ChatSettings {
                temperature: config.model.chat_temperature,
                max_tokens: config.model.chat_max_tokens,
            },
            hub: hub.clone(),
            router: Arc::clone(&router),
            charter_path: config.host.data_dir.join(format!("{id}.charter.md")),
            decisions_path: config.host.data_dir.join(format!("{id}.decisions.md")),
        });
        let wiring = Wiring {
            journal,
            inputs: rx,
            wipes: wipe_rx,
            self_tx: tx,
            outbound: outbound.clone(),
            model: Arc::clone(&model),
            classifier: Arc::clone(&classifier),
            extractor: Arc::clone(&extractor),
            resolver: Arc::clone(&resolver),
            mailer: mailer.clone(),
            strict_schema: dialect == Dialect::OpenAi,
            chat: crate::runtime::model::ChatSettings {
                temperature: config.model.chat_temperature,
                max_tokens: config.model.chat_max_tokens,
            },
            hub: hub.clone(),
            router: Arc::clone(&router),
            charter_path: config.host.data_dir.join(format!("{id}.charter.md")),
            decisions_path: config.host.data_dir.join(format!("{id}.decisions.md")),
        };
        let runner = GroupRunner::recover(id.clone(), desired.clone(), wiring).await?;
        let shutdown = shutdown_rx.clone();
        let again = Arc::clone(&wiring_parts);
        let store = Arc::clone(&store);
        let hub = hub.clone();
        let data_dir = config.host.data_dir.clone();
        tasks.push(tokio::spawn(async move {
            let mut runner = runner;
            loop {
                match runner.run(shutdown.clone()).await {
                    Ok(RunEnd::Shutdown) => break,
                    Ok(RunEnd::Wiped { inputs, wipes, done }) => {
                        // The runner is gone, and with it every handle on the file.
                        let fresh: Result<Journal<Input, GroupState>, String> =
                            wipe_group(&store, &hub, &data_dir, &id).and_then(|()| {
                                store
                                    .open_group(&id)
                                    .map(|log| Journal::new(log))
                                    .map_err(|e| e.to_string())
                            });
                        let journal = match fresh {
                            Ok(journal) => journal,
                            Err(error) => {
                                tracing::error!(group = %id, %error, "wipe failed; the group is down");
                                let _ = done.send(Err(error));
                                break;
                            }
                        };
                        if let Ok(secret) = journal.feed_secret() {
                            hub.set_feed_secret(&id, secret);
                        }
                        match GroupRunner::recover(id.clone(), desired.clone(), again.wiring(journal, inputs, wipes)).await {
                            Ok(next) => {
                                tracing::warn!(group = %id, "wiped and started fresh");
                                let _ = done.send(Ok(()));
                                runner = next;
                            }
                            Err(error) => {
                                tracing::error!(group = %id, %error, "could not start the wiped group");
                                let _ = done.send(Err(error.to_string()));
                                break;
                            }
                        }
                    }
                    Err(error) => {
                        tracing::error!(group = %id, %error, "group stopped");
                        break;
                    }
                }
            }
        }));
    }

    // Mail has no room, so every group hears it; each engine keeps only what
    // answers something it sent.
    if let Some(inbox) = inbox {
        let (mail_tx, mut mail_rx) = mpsc::channel(64);
        let every = config
            .mail
            .as_ref()
            .and_then(|m| m.imap.as_ref())
            .map_or(std::time::Duration::from_secs(60), |i| i.interval());
        tasks.push(tokio::spawn(ours_mail::poll_loop(inbox, every, mail_tx)));
        let queues: Vec<mpsc::Sender<Input>> = group_queues.clone();
        let own = config.mail.as_ref().map(|m| address_in(&m.from));
        tasks.push(tokio::spawn(async move {
            while let Some(mail) = mail_rx.recv().await {
                if own.as_deref() == Some(address_in(&mail.from).as_str()) {
                    continue;
                }
                for queue in &queues {
                    let _ = queue.send(Input::Mail(mail.clone())).await;
                }
            }
        }));
    }

    for (platform, mut rx) in inbound {
        let router = Arc::clone(&router);
        tasks.push(tokio::spawn(async move {
            while let Some(event) = rx.recv().await {
                router.dispatch(platform, event).await;
            }
        }));
    }

    // The opt-in telemetry job: inert unless the host turned it on.
    tasks.push(tokio::spawn(crate::telemetry::job(
        Arc::clone(&store),
        config.telemetry.clone(),
        config.host.data_dir.clone(),
        config
            .groups
            .iter()
            .map(|g| GroupId::new(g.id.clone()))
            .collect(),
        std::time::Duration::from_secs(3600),
    )));

    hub.set_store(Arc::clone(&store));
    hub.set_host_facts(host_facts(&config));
    let admin = AdminServer::new(AdminConfig {
        bind: config.host.admin_bind,
        token: config.host.admin_token.clone(),
    })?;
    let url = match lan_ip() {
        Some(ip) if config.host.admin_bind.ip().is_unspecified() => admin.url_with_host(ip),
        _ => admin.url(),
    };
    tracing::info!(%url, "admin page");
    for line in qr_lines_for(&url) {
        tracing::info!("{line}");
    }
    let source: Arc<dyn AdminSource> = Arc::new(hub.clone());
    tasks.push(tokio::spawn(async move {
        if let Err(error) = admin.serve(source).await {
            tracing::error!(%error, "admin page stopped");
        }
    }));

    let mut tripped = shutdown_rx.clone();
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    tokio::select! {
        _ = tokio::signal::ctrl_c() => tracing::info!("stopping"),
        _ = terminate.recv() => tracing::info!("stopping on SIGTERM"),
        _ = tripped.changed() => tracing::error!("stopping because a connector stopped"),
    }
    let _ = shutdown_tx.send(true);
    let _ = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        for task in tasks {
            let _ = task.await;
        }
    })
    .await;
    if connector_died.load(std::sync::atomic::Ordering::SeqCst) {
        anyhow::bail!("a connector stopped; exiting so a supervisor restarts the agent");
    }
    Ok(())
}

/// The large-model client exactly as `run` uses it, and its dialect.
pub fn build_model(config: &Config) -> anyhow::Result<(Arc<dyn LargeModel>, Dialect)> {
    let meter = Arc::new(SpendMeter::new(config.model.daily_cap_usd));
    let dialect = match config.model.dialect {
        ModelDialect::OpenAi => Dialect::OpenAi,
        ModelDialect::LlamaCpp => Dialect::LlamaCpp,
        ModelDialect::Ollama => Dialect::Ollama,
    };
    let mut model_config =
        ModelConfig::new(config.model.base_url.clone(), config.model.model.clone());
    model_config.api_key = config.model.api_key.clone();
    model_config.dialect = dialect;
    model_config.timeout_secs = config.model.timeout_secs;
    model_config.extra = config.model.extra.clone();
    let model: Arc<dyn LargeModel> = Arc::new(OpenAiCompatible::new(model_config, meter)?);
    Ok((model, dialect))
}

/// The classifier, the extractor and the time resolver, all on the small model: the `[model]`
/// endpoint unless the `[classifier]` section points elsewhere, or the rules
/// pass and no extraction when it is disabled.
#[allow(clippy::type_complexity)]
pub fn build_small_models(
    config: &Config,
    model: &Arc<dyn LargeModel>,
    dialect: Dialect,
) -> anyhow::Result<(
    Arc<dyn Classifier>,
    Arc<dyn Extractor>,
    Arc<dyn TimeResolver>,
)> {
    let section = &config.classifier;
    if !section.enabled {
        return Ok((
            Arc::new(RulesOnlyClassifier),
            Arc::new(NoExtractor),
            Arc::new(NoTimeResolver),
        ));
    }
    let same_endpoint =
        section.base_url.is_none() && section.model.is_none() && section.dialect.is_none();
    let small: Arc<dyn LargeModel> = if same_endpoint {
        Arc::clone(model)
    } else {
        let mut model_config = ModelConfig::new(
            section
                .base_url
                .clone()
                .unwrap_or_else(|| config.model.base_url.clone()),
            section
                .model
                .clone()
                .unwrap_or_else(|| config.model.model.clone()),
        );
        model_config.api_key = section
            .api_key
            .clone()
            .or_else(|| config.model.api_key.clone());
        model_config.dialect = section.dialect.map_or(dialect, |d| match d {
            ModelDialect::OpenAi => Dialect::OpenAi,
            ModelDialect::LlamaCpp => Dialect::LlamaCpp,
            ModelDialect::Ollama => Dialect::Ollama,
        });
        model_config.timeout_secs = config.model.timeout_secs;
        model_config.extra = section
            .extra
            .clone()
            .unwrap_or_else(|| config.model.extra.clone());
        let meter = Arc::new(SpendMeter::new(config.model.daily_cap_usd));
        Arc::new(OpenAiCompatible::new(model_config, meter)?)
    };
    Ok((
        Arc::new(SmallModelClassifier::new(Arc::clone(&small))),
        Arc::new(SmallModelExtractor::new(Arc::clone(&small))),
        Arc::new(SmallModelTimeResolver::new(small)),
    ))
}

/// Sends one canned addressed message through the configured model with the
/// engine's plan schema and returns the plan, so a host can prove the
/// endpoint works before adding the bot to a room.
pub async fn model_check(config: &Config) -> anyhow::Result<ours_core::Plan> {
    let (model, dialect) = build_model(config)?;
    let request = ours_core::ModelRequest {
        id: ours_types::RequestId::new("model-check"),
        system: "You are Q, a member of a friends group chat called The Six. Plain text, two lines at most.".to_owned(),
        messages: vec![ours_core::ChatTurn {
            role: ours_core::TurnRole::User,
            content: "alex: Q, what was that bar we went to in March?".to_owned(),
        }],
        output_schema: ours_core::plan_schema(),
    };
    let started = std::time::Instant::now();
    let plan = crate::runtime::model::ask(
        model.as_ref(),
        &GroupId::new("model-check"),
        request,
        dialect == Dialect::OpenAi,
        crate::runtime::model::ChatSettings {
            temperature: config.model.chat_temperature,
            max_tokens: config.model.chat_max_tokens,
        },
    )
    .await
    .map_err(|e| anyhow::anyhow!(e))?;
    tracing::info!(
        model = model.name(),
        ms = started.elapsed().as_millis() as u64,
        "model answered"
    );
    Ok(plan)
}

/// The group's mail account: SMTP and IMAP from `[mail]`, or the file-based
/// pair under `dir` (out/ for sent mail, in/ for replies dropped there), or
/// nothing at all.
#[allow(clippy::type_complexity)]
pub fn build_mail(
    config: &Config,
) -> anyhow::Result<(Option<Arc<dyn Mailer>>, Option<Arc<dyn Inbox>>)> {
    let Some(section) = &config.mail else {
        return Ok((None, None));
    };
    let mail_config = ours_mail::MailConfig {
        from: section.from.clone(),
        reply_to: section.reply_to.clone(),
        smtp: section
            .smtp
            .clone()
            .unwrap_or_else(|| ours_mail::SmtpConfig {
                host: String::new(),
                port: 0,
                username: String::new(),
                password: String::new(),
                starttls: true,
            }),
        imap: section.imap.clone(),
    };
    if let Some(dir) = &section.dir {
        std::fs::create_dir_all(dir.join("in"))?;
        let mailer: Arc<dyn Mailer> =
            Arc::new(ours_mail::FileMailer::new(dir.join("out"), &mail_config)?);
        let inbox: Arc<dyn Inbox> = Arc::new(ours_mail::DirInbox::new(dir.join("in")));
        tracing::info!(dir = %dir.display(), "mail in file mode");
        return Ok((Some(mailer), Some(inbox)));
    }
    let mailer: Arc<dyn Mailer> = Arc::new(ours_mail::SmtpMailer::new(&mail_config)?);
    let inbox: Option<Arc<dyn Inbox>> = match section.imap {
        Some(_) => Some(Arc::new(ours_mail::ImapInbox::new(&mail_config)?)),
        None => None,
    };
    Ok((Some(mailer), inbox))
}

/// The address a phone on the same network can reach; found by asking the
/// OS which interface would route to the internet. No packet is sent.
fn lan_ip() -> Option<IpAddr> {
    let socket = UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("1.1.1.1:80").ok()?;
    socket.local_addr().ok().map(|a| a.ip())
}

/// Everything a group runner needs that survives a wipe.
struct WiringParts {
    self_tx: mpsc::Sender<Input>,
    outbound: HashMap<Platform, mpsc::Sender<Outbound>>,
    model: Arc<dyn LargeModel>,
    classifier: Arc<dyn Classifier>,
    extractor: Arc<dyn Extractor>,
    resolver: Arc<dyn TimeResolver>,
    mailer: Option<Arc<dyn Mailer>>,
    strict_schema: bool,
    chat: crate::runtime::model::ChatSettings,
    hub: AdminHub,
    router: Arc<Router>,
    charter_path: PathBuf,
    decisions_path: PathBuf,
}

impl WiringParts {
    fn wiring(
        &self,
        journal: Journal<Input, GroupState>,
        inputs: mpsc::Receiver<Input>,
        wipes: mpsc::Receiver<WipeRequest>,
    ) -> Wiring {
        Wiring {
            journal,
            inputs,
            wipes,
            self_tx: self.self_tx.clone(),
            outbound: self.outbound.clone(),
            model: Arc::clone(&self.model),
            classifier: Arc::clone(&self.classifier),
            extractor: Arc::clone(&self.extractor),
            resolver: Arc::clone(&self.resolver),
            mailer: self.mailer.clone(),
            strict_schema: self.strict_schema,
            chat: self.chat,
            hub: self.hub.clone(),
            router: Arc::clone(&self.router),
            charter_path: self.charter_path.clone(),
            decisions_path: self.decisions_path.clone(),
        }
    }
}

/// Removes everything on disk that belongs to one group: the journal, its
/// write-ahead files, the rendered charter and decision log.
pub fn wipe_group(
    store: &StoreDir,
    hub: &AdminHub,
    data_dir: &Path,
    id: &GroupId,
) -> Result<(), String> {
    store.wipe_group(id).map_err(|e| e.to_string())?;
    for name in [format!("{id}.charter.md"), format!("{id}.decisions.md")] {
        let path = data_dir.join(name);
        if path.exists() {
            std::fs::remove_file(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        }
    }
    hub.clear_group(id);
    Ok(())
}

/// What the compliance page states about this host, from the configuration.
fn host_facts(config: &Config) -> ours_admin::HostFacts {
    let endpoint = config.model.base_url.clone();
    let local = reqwest::Url::parse(&endpoint)
        .ok()
        .and_then(|u| u.host_str().map(str::to_owned))
        .is_some_and(|h| {
            h == "localhost"
                || h.parse::<IpAddr>().is_ok_and(|ip| {
                    ip.is_loopback()
                        || match ip {
                            IpAddr::V4(v4) => v4.is_private() || v4.is_link_local(),
                            IpAddr::V6(v6) => v6.is_unique_local() || v6.is_unicast_link_local(),
                        }
                })
        });
    ours_admin::HostFacts {
        version: crate::VERSION.to_owned(),
        data_dir: config.host.data_dir.display().to_string(),
        admin_bind: config.host.admin_bind.to_string(),
        model: ours_admin::ModelFacts {
            label: config.model.model.clone(),
            endpoint,
            local,
        },
        mail: config.mail.as_ref().map(|m| m.from.clone()),
        telemetry: ours_admin::TelemetryFacts {
            enabled: config.telemetry.enabled,
            endpoint: config.telemetry.endpoint.clone(),
            sample: crate::telemetry::sample(&[crate::telemetry::example()], 1),
        },
        connectors: config
            .connectors
            .iter()
            .map(|c| format!("{:?}", c.platform).to_lowercase())
            .collect(),
    }
}

/// The bare address in a From line: "Q <q@example.org>" gives "q@example.org".
fn address_in(from: &str) -> String {
    let inner = match (from.find('<'), from.rfind('>')) {
        (Some(a), Some(b)) if a < b => &from[a + 1..b],
        _ => from,
    };
    inner.trim().trim_start_matches("mailto:").to_lowercase()
}

#[cfg(test)]
mod address_tests {
    use super::address_in;

    #[test]
    fn the_address_is_compared_whole() {
        assert_eq!(address_in("Ours <ours@example.org>"), "ours@example.org");
        assert_eq!(address_in("rum@example.org"), "rum@example.org");
        assert_ne!(
            address_in("Ours <ours@example.org>"),
            address_in("rum@example.org")
        );
    }
}
