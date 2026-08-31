use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use clap::Parser;
use tokio::sync::{broadcast, watch};
use tt_cli::logging;

use tt_server::ServerEvent;
use tt_server::api::{ApiState, router};
use tt_server::loops::{LoopRuntime, LoopRuntimeConfig};

#[derive(Debug, Parser)]
#[command(
    name = "tt-serve",
    version,
    about = "Run the tt background daemon and local API"
)]
struct Args {
    #[arg(long)]
    port: Option<u16>,
    #[arg(long, value_name = "PATH")]
    db: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    // The daemon runs under systemd with no `RUST_LOG`, so warnings must be on by
    // default or its only account of a failed classification is the count.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(logging::filter(0))
        .try_init();

    let mut config = tt_cli::Config::load().context("load configuration")?;
    if let Some(database_path) = args.db {
        config.database_path = database_path;
    }
    let port = args.port.unwrap_or(config.serve.port);
    let classifier = configure_classifier(&config);
    let (events, _) = broadcast::channel::<ServerEvent>(1024);
    let app = router(ApiState {
        database_path: config.database_path.clone(),
        config: config.clone(),
        events: events.clone(),
    });
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let loops = LoopRuntime::new(LoopRuntimeConfig {
        database_path: config.database_path.clone(),
        config,
        classifier: classifier.map(|classifier| classifier as Arc<dyn tt_llm::Classifier>),
        events,
    });
    let loop_task = tokio::spawn(loops.run(shutdown_rx));
    let address = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port);
    let listener = tokio::net::TcpListener::bind(address)
        .await
        .context("bind HTTP listener")?;
    println!(
        "listening on {}",
        listener.local_addr().context("read listener address")?
    );
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("serve HTTP API")?;
    shutdown_tx
        .send(true)
        .map_err(|_| anyhow::anyhow!("daemon loop shutdown receiver dropped"))?;
    loop_task.await.context("join daemon loops")?;
    Ok(())
}

async fn shutdown_signal() {
    if let Err(error) = tokio::signal::ctrl_c().await {
        tracing::error!(%error, "install Ctrl-C handler");
    }
}

fn configure_classifier(config: &tt_cli::Config) -> Option<Arc<tt_llm::RigClassifier>> {
    match tt_cli::commands::classify_auto::build_classifier(config) {
        Ok(classifier) => {
            persist_classifier_state(config, None);
            Some(Arc::new(classifier))
        }
        Err(error) => {
            let detail = logging::chain(&error);
            if let Some(tt_llm::LlmError::MissingApiKey(variable)) =
                error.downcast_ref::<tt_llm::LlmError>()
            {
                tracing::warn!(
                    api_key_env = %variable,
                    "classifier unavailable; environment variable is not set; classification loop disabled"
                );
            } else {
                tracing::warn!(
                    error = %detail,
                    "classifier unavailable; configuration failed; classification loop disabled"
                );
            }
            persist_classifier_state(config, Some(&detail));
            None
        }
    }
}

fn persist_classifier_state(config: &tt_cli::Config, unavailable_error: Option<&str>) {
    let db = match tt_db::Database::open(&config.database_path) {
        Ok(db) => db,
        Err(error) => {
            tracing::warn!(%error, "classifier health persistence unavailable");
            return;
        }
    };
    let result = unavailable_error.map_or_else(
        || db.record_classifier_ready(),
        |error| db.record_classifier_unconfigured(error),
    );
    if let Err(error) = result {
        tracing::warn!(%error, "classifier health persistence failed");
    }
}

#[cfg(test)]
mod tests {
    use std::process::Command;
    use std::time::Duration;

    use anyhow::Result;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};
    use tokio::sync::{broadcast, watch};
    use tt_cli::Config;
    use tt_db::{ClassifierHealthState, Database};

    use super::{
        ApiState, LoopRuntime, LoopRuntimeConfig, ServerEvent, configure_classifier, router,
    };

    #[tokio::test]
    async fn daemon_runs_without_a_configured_classifier_and_reports_unconfigured_health()
    -> Result<()> {
        // Given: a file-backed database and a classifier environment variable that is absent.
        let database_file = tempfile::NamedTempFile::new()?;
        let temp = tempfile::tempdir()?;
        let config = Config {
            database_path: database_file.path().to_path_buf(),
            todo_store_path: temp.path().join("todos"),
            ..Config::default()
        };
        let mut config = config;
        config.classifier.api_key_env = "TT_SERVER_TEST_MISSING_ANTHROPIC_API_KEY".to_owned();
        assert!(std::env::var_os(&config.classifier.api_key_env).is_none());

        // When: daemon components are created without an available classifier.
        let classifier = configure_classifier(&config);
        let health = Database::open(database_file.path())?.get_classifier_health()?;
        let (events, _) = broadcast::channel::<ServerEvent>(1);
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let loop_task = tokio::spawn(
            LoopRuntime::new(LoopRuntimeConfig {
                database_path: database_file.path().to_path_buf(),
                config: config.clone(),
                classifier: classifier
                    .clone()
                    .map(|classifier| classifier as std::sync::Arc<dyn tt_llm::Classifier>),
                events: events.clone(),
            })
            .run(shutdown_rx.clone()),
        );
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let app = router(ApiState {
            database_path: database_file.path().to_path_buf(),
            config,
            events,
        });
        let server_task = tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(shutdown_requested(shutdown_rx))
                .await
        });
        tokio::task::yield_now().await;
        let response = status_response(&address).await?;
        shutdown_tx.send(true)?;

        // Then: non-classifier loops stay alive and the status API exposes the degraded state.
        assert!(classifier.is_none());
        assert_eq!(health.state, ClassifierHealthState::Unconfigured);
        assert!(!loop_task.is_finished());
        assert!(response.starts_with("HTTP/1.1 200"));
        assert!(response.contains("\"state\":\"unconfigured\""));
        tokio::time::timeout(Duration::from_secs(1), loop_task).await??;
        tokio::time::timeout(Duration::from_secs(1), server_task).await???;
        Ok(())
    }

    #[test]
    fn daemon_configured_context_command_enables_context_lookup() {
        const CHILD_MARKER: &str = "TT_SERVER_CONTEXT_LOOKUP_TEST_CHILD";
        const API_KEY_ENV: &str = "TT_SERVER_CONTEXT_LOOKUP_TEST_API_KEY";

        if std::env::var_os(CHILD_MARKER).is_some() {
            let database_file = tempfile::NamedTempFile::new().unwrap();
            let temp = tempfile::tempdir().unwrap();
            let mut config = Config {
                database_path: database_file.path().to_path_buf(),
                todo_store_path: temp.path().join("todos"),
                ..Config::default()
            };
            config.classifier.api_key_env = API_KEY_ENV.to_owned();
            config.classifier.context_command = Some("printf".to_owned());

            let classifier = configure_classifier(&config).expect("configured classifier");

            assert!(classifier.context_lookup_enabled());
            assert_eq!(
                Database::open(database_file.path())
                    .unwrap()
                    .get_classifier_health()
                    .unwrap()
                    .state,
                ClassifierHealthState::Ready
            );
            return;
        }

        let output = Command::new(std::env::current_exe().unwrap())
            .arg("--exact")
            .arg("tests::daemon_configured_context_command_enables_context_lookup")
            .arg("--nocapture")
            .env(CHILD_MARKER, "1")
            .env(API_KEY_ENV, "test-key")
            .output()
            .unwrap();

        assert!(
            output.status.success(),
            "child test failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn malformed_daemon_context_command_disables_classifier_and_records_health() {
        const CHILD_MARKER: &str = "TT_SERVER_MALFORMED_CONTEXT_COMMAND_TEST_CHILD";
        const API_KEY_ENV: &str = "TT_SERVER_MALFORMED_CONTEXT_COMMAND_TEST_API_KEY";

        if std::env::var_os(CHILD_MARKER).is_some() {
            let database_file = tempfile::NamedTempFile::new().unwrap();
            let temp = tempfile::tempdir().unwrap();
            let mut config = Config {
                database_path: database_file.path().to_path_buf(),
                todo_store_path: temp.path().join("todos"),
                ..Config::default()
            };
            config.classifier.api_key_env = API_KEY_ENV.to_owned();
            config.classifier.context_command = Some("'".to_owned());

            assert!(configure_classifier(&config).is_none());
            let health = Database::open(database_file.path())
                .unwrap()
                .get_classifier_health()
                .unwrap();
            assert_eq!(health.state, ClassifierHealthState::Unconfigured);
            assert!(
                health
                    .last_error
                    .as_deref()
                    .is_some_and(|detail| detail.contains("unmatched quote")),
                "{health:?}"
            );
            return;
        }

        let output = Command::new(std::env::current_exe().unwrap())
            .arg("--exact")
            .arg("tests::malformed_daemon_context_command_disables_classifier_and_records_health")
            .arg("--nocapture")
            .env(CHILD_MARKER, "1")
            .env(API_KEY_ENV, "test-key")
            .output()
            .unwrap();

        assert!(
            output.status.success(),
            "child test failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    async fn shutdown_requested(mut shutdown: watch::Receiver<bool>) {
        let _ = shutdown.changed().await;
    }

    async fn status_response(address: &std::net::SocketAddr) -> Result<String> {
        let mut stream = TcpStream::connect(address).await?;
        stream
            .write_all(b"GET /api/status HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
            .await?;
        let mut response = String::new();
        stream.read_to_string(&mut response).await?;
        Ok(response)
    }
}
