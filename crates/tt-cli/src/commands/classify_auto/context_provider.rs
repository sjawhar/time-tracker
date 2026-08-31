//! Read-only command access for automatic-classifier context lookup.
//!
//! `tt-llm` owns the model-facing tool contract and budget. This module turns optional
//! `[classifier]` command configuration into that contract: it splits the configured command
//! with shell-word rules, appends the model query as one argument, bounds wall clock and
//! returned text, and translates every subprocess problem into a tool result.

use std::io::Read;
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use tt_llm::{ContextProvider, ContextProviderError};

/// Wall-clock budget for one local context-lookup command.
const CONTEXT_LOOKUP_TIMEOUT: Duration = Duration::from_secs(10);

/// Largest model-facing result, including the truncation marker when needed.
pub const MAX_CONTEXT_LOOKUP_RESULT_BYTES: usize = 4 * 1024;

const TRUNCATION_MARKER: &str = "\n[context lookup output truncated]";
const CAPTURE_BYTES: usize = MAX_CONTEXT_LOOKUP_RESULT_BYTES + 4;

/// The operator-configured command that answers context lookups.
pub struct CommandContextProvider {
    program: PathBuf,
    args: Vec<String>,
    timeout: Duration,
}

impl CommandContextProvider {
    /// Parses a shell-word command line into its program and fixed arguments.
    ///
    /// # Errors
    /// When the command is empty or contains an unmatched quote or escape.
    pub fn new(command: &str) -> Result<Self, ContextProviderError> {
        let words = shlex::split(command).ok_or_else(|| {
            ContextProviderError::Backend(
                "context command contains an unmatched quote or escape".to_owned(),
            )
        })?;
        let Some((program, args)) = words.split_first() else {
            return Err(ContextProviderError::Backend(
                "context command must name a program".to_owned(),
            ));
        };
        Ok(Self {
            program: PathBuf::from(program),
            args: args.to_vec(),
            timeout: CONTEXT_LOOKUP_TIMEOUT,
        })
    }

    /// Same contract as [`Self::new`] with a caller-chosen wall-clock budget.
    ///
    /// Exists so tests can prove the timeout kills a hung command without paying
    /// the production budget in real time.
    ///
    /// # Errors
    /// When the command is empty or contains an unmatched quote or escape.
    #[cfg(test)]
    pub fn with_timeout(command: &str, timeout: Duration) -> Result<Self, ContextProviderError> {
        let mut provider = Self::new(command)?;
        provider.timeout = timeout;
        Ok(provider)
    }

    fn execute(&self, query: &str) -> Result<String, ContextProviderError> {
        let mut command = Command::new(&self.program);
        command
            .args(&self.args)
            .arg(query)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(unix)]
        command.process_group(0);
        let mut child = command.spawn().map_err(|error| {
            ContextProviderError::Backend(format!("context command could not start: {error}"))
        })?;
        let stdout = child.stdout.take().ok_or_else(|| {
            ContextProviderError::Backend("context command stdout pipe was unavailable".to_owned())
        })?;
        let stderr = child.stderr.take().ok_or_else(|| {
            ContextProviderError::Backend("context command stderr pipe was unavailable".to_owned())
        })?;
        let stdout_reader = thread::spawn(move || drain_capped(stdout));
        let stderr_reader = thread::spawn(move || drain_capped(stderr));

        let deadline = Instant::now() + self.timeout;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) if Instant::now() >= deadline => {
                    terminate_process_group(&mut child);
                    return Err(timeout_error(self.timeout));
                }
                Ok(None) => thread::sleep(Duration::from_millis(10)),
                Err(error) => {
                    terminate_process_group(&mut child);
                    return Err(ContextProviderError::Backend(format!(
                        "context command could not check its status: {error}"
                    )));
                }
            }
        };
        if !readers_finished_by(&stdout_reader, &stderr_reader, deadline) {
            terminate_process_group(&mut child);
            return Err(timeout_error(self.timeout));
        }
        let stdout = join_reader(stdout_reader, "stdout");
        let stderr = join_reader(stderr_reader, "stderr");
        let (stdout, stderr) = match (stdout, stderr) {
            (Ok(stdout), Ok(stderr)) => (stdout, stderr),
            (Err(error), _) | (_, Err(error)) => {
                terminate_process_group(&mut child);
                return Err(error);
            }
        };
        if !status.success() {
            terminate_process_group(&mut child);
            let stderr = bounded_text(stderr, "stderr")?;
            let detail = stderr.trim();
            let detail = if detail.is_empty() {
                "no error output"
            } else {
                detail
            };
            return Err(ContextProviderError::Backend(format!(
                "context command failed with {status}: {detail}"
            )));
        }
        bounded_text(stdout, "stdout")
    }
}

fn timeout_error(timeout: Duration) -> ContextProviderError {
    ContextProviderError::Backend(format!(
        "context command timed out after {:.1} seconds",
        timeout.as_secs_f32()
    ))
}

fn terminate_process_group(child: &mut Child) {
    #[cfg(unix)]
    {
        let process_group = format!("-{}", child.id());
        if let Err(error) = Command::new("kill")
            .args(["-KILL", "--", &process_group])
            .status()
        {
            tracing::debug!(%error, process_group, "could not terminate context command process group");
        }
    }
    #[cfg(not(unix))]
    {
        let _ = child.kill();
    }
    let _ = child.wait();
}

fn readers_finished_by(
    stdout_reader: &thread::JoinHandle<std::io::Result<Vec<u8>>>,
    stderr_reader: &thread::JoinHandle<std::io::Result<Vec<u8>>>,
    deadline: Instant,
) -> bool {
    while !stdout_reader.is_finished() || !stderr_reader.is_finished() {
        if Instant::now() >= deadline {
            return false;
        }
        thread::sleep(Duration::from_millis(10));
    }
    true
}

impl ContextProvider for CommandContextProvider {
    fn lookup(&self, query: &str) -> Result<String, ContextProviderError> {
        self.execute(query)
    }
}

fn drain_capped(mut reader: impl Read) -> std::io::Result<Vec<u8>> {
    let mut captured = Vec::with_capacity(CAPTURE_BYTES);
    let mut buffer = [0_u8; 4096];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            return Ok(captured);
        }
        let remaining = CAPTURE_BYTES.saturating_sub(captured.len());
        captured.extend(&buffer[..read.min(remaining)]);
    }
}

fn join_reader(
    reader: thread::JoinHandle<std::io::Result<Vec<u8>>>,
    stream: &str,
) -> Result<Vec<u8>, ContextProviderError> {
    reader
        .join()
        .map_err(|_| {
            ContextProviderError::Backend(format!("context command {stream} reader panicked"))
        })?
        .map_err(|error| {
            ContextProviderError::Backend(format!(
                "context command could not read {stream}: {error}"
            ))
        })
}

fn bounded_text(bytes: Vec<u8>, stream: &str) -> Result<String, ContextProviderError> {
    let truncated = bytes.len() > MAX_CONTEXT_LOOKUP_RESULT_BYTES;
    let mut text = match String::from_utf8(bytes) {
        Ok(text) => text,
        Err(error) if truncated && error.utf8_error().error_len().is_none() => {
            let valid_prefix = error.utf8_error().valid_up_to();
            let mut bytes = error.into_bytes();
            bytes.truncate(valid_prefix);
            String::from_utf8(bytes).expect("a UTF-8 error's valid prefix is valid UTF-8")
        }
        Err(error) => {
            return Err(ContextProviderError::Backend(format!(
                "context command emitted non-UTF-8 {stream}: {error}"
            )));
        }
    };
    if !truncated {
        return Ok(text);
    }

    let mut end = (MAX_CONTEXT_LOOKUP_RESULT_BYTES - TRUNCATION_MARKER.len()).min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text.truncate(end);
    text.push_str(TRUNCATION_MARKER);
    Ok(text)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::Write;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use tempfile::TempDir;
    use tt_llm::{
        ClassificationInput, ClassificationOutput, Classifier, ContextLookupRequest,
        ContextProvider, ContextProviderTools, MockClassifier, StreamChoice,
    };

    use super::{CommandContextProvider, MAX_CONTEXT_LOOKUP_RESULT_BYTES};

    #[cfg(unix)]
    /// Writes a shell fixture and returns a command line that runs it via `sh`.
    ///
    /// Deliberately NOT executed directly: exec-ing a freshly written file races the
    /// fork storm of a parallel test run — a sibling thread's fork can hold the script's
    /// write fd between fork and exec, and the spawn fails with ETXTBSY. `sh` opens the
    /// script as data, which cannot trip that race.
    fn lookup_command(temp: &TempDir, body: &str) -> String {
        let path = temp.path().join("lookup-fixture.sh");
        let mut file = fs::File::create(&path).unwrap();
        file.write_all(format!("{body}\n").as_bytes()).unwrap();
        file.sync_all().unwrap();
        drop(file);
        format!("sh {}", path.display())
    }

    fn input() -> ClassificationInput {
        ClassificationInput {
            has_session: true,
            session_id: "session-1".to_owned(),
            machine: None,
            cwd: None,
            starting_prompt: Some("Classify example-initiative work".to_owned()),
            user_prompts: Vec::new(),
            window_titles: Vec::new(),
            started_at: None,
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_failed_lookup_command_is_a_clean_tool_result_and_the_classification_continues() {
        // Given: a configured command that refuses its read.
        let temp = TempDir::new().unwrap();
        let command = lookup_command(&temp, "printf 'lookup is unavailable\\n' >&2\nexit 17");
        let tools =
            ContextProviderTools::new(Arc::new(CommandContextProvider::new(&command).unwrap()));
        let classifier = MockClassifier {
            brain: Some(Box::new(|_input, _session, context_lookup| {
                let rendered = context_lookup
                    .expect("a configured provider must be offered")
                    .dispatch(&ContextLookupRequest {
                        query: "example-initiative".to_owned(),
                    })
                    .rendered();
                assert!(rendered.contains("context command failed"), "{rendered}");
                assert!(rendered.contains("lookup is unavailable"), "{rendered}");
                Ok(ClassificationOutput {
                    choice: StreamChoice::Existing {
                        stream_id: "example-initiative".to_owned(),
                    },
                    confidence: 0.9,
                    reasoning: "command failure left the normal verdict path available".to_owned(),
                })
            })),
            context_provider: Some(tools),
            ..MockClassifier::default()
        };

        // When
        let output = classifier.classify(&input(), &[], None).unwrap();

        // Then: tool trouble is evidence the model can reason around, never a classifier
        // failure that abandons the session.
        assert_eq!(
            output.choice,
            StreamChoice::Existing {
                stream_id: "example-initiative".to_owned(),
            }
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_lookup_command_timeout_kills_a_non_exec_wrapper_and_returns_promptly() {
        // Given: a wrapper that waits on a grandchild which inherited the output pipes.
        // Killing only the wrapper leaves the reader threads blocked until that child exits.
        let temp = TempDir::new().unwrap();
        let command = lookup_command(&temp, "sleep 2 & wait");
        let provider =
            CommandContextProvider::with_timeout(&command, Duration::from_millis(100)).unwrap();

        // When
        let started = Instant::now();
        let error = provider.lookup("example-initiative").unwrap_err();

        // Then: the configured timeout limits the whole process tree rather than merely
        // terminating the shell that started it.
        assert!(
            error.to_string().contains("timed out after 0.1 seconds"),
            "{error}"
        );
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[cfg(unix)]
    #[test]
    fn lookup_results_are_truncated_with_a_marker() {
        // Given: a valid read that emits more text than a model should receive from one tool.
        let temp = TempDir::new().unwrap();
        let command = lookup_command(&temp, "printf '%05000d' 0");
        let provider = CommandContextProvider::new(&command).unwrap();

        // When
        let text = provider.lookup("example-initiative").unwrap();

        // Then: result text has a strict byte ceiling and tells the model that it is partial.
        assert!(
            text.len() <= MAX_CONTEXT_LOOKUP_RESULT_BYTES,
            "{} bytes",
            text.len()
        );
        assert!(
            text.ends_with("[context lookup output truncated]"),
            "{text}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn valid_multibyte_lookup_output_split_by_the_capture_cap_is_truncated() {
        // Given: valid UTF-8 longer than the capture buffer, where the byte cap ends in
        // the middle of one three-byte character.
        let temp = TempDir::new().unwrap();
        let command = lookup_command(&temp, "printf '€%.0s' $(seq 1 2000)");
        let provider = CommandContextProvider::new(&command).unwrap();

        // When
        let text = provider.lookup("example-initiative").unwrap();

        // Then: the valid prefix survives and explicitly tells the model that it is partial.
        assert!(
            text.len() <= MAX_CONTEXT_LOOKUP_RESULT_BYTES,
            "{}",
            text.len()
        );
        assert!(
            text.ends_with("[context lookup output truncated]"),
            "{text}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn configured_lookup_command_appends_the_query_as_one_argument() {
        // Given: an operator command with a quoted base argument.
        let temp = TempDir::new().unwrap();
        let command = format!(
            "{} --source 'operator notes'",
            lookup_command(&temp, "printf '%s\\n' \"$@\"")
        );
        let provider = CommandContextProvider::new(&command).unwrap();

        // When
        let text = provider.lookup("sample migration initiative").unwrap();

        // Then: the configured arguments stay intact and the model query occupies one,
        // final argv element.
        assert_eq!(
            text,
            "--source\noperator notes\nsample migration initiative\n"
        );
    }
}
