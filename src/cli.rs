// CLI output is the product, not telemetry — printing to stdout/stderr is the point.
#![allow(clippy::print_stdout, clippy::print_stderr)]
// `cli` is a private module of the binary; `unreachable_pub` and `redundant_pub_crate`
// disagree on the right visibility, so we let the binary expose it as `pub`.
#![allow(unreachable_pub)]

use anyhow::Result;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio_util::sync::CancellationToken;

use relay_rs::Agent;
use relay_rs::types::Prompt;

pub async fn run(agent: Agent) -> Result<()> {
    let session = agent.start_session().await?;
    let stdin = tokio::io::stdin();
    let mut reader = BufReader::new(stdin).lines();
    let mut stdout = tokio::io::stdout();

    stdout
        .write_all(b"relay-rs - type a prompt, blank line to send, Ctrl-D to exit, Ctrl-C to cancel a reply.\n")
        .await?;

    loop {
        stdout.write_all(b"\n> ").await?;
        stdout.flush().await?;

        let buf = match read_until_blank_line(&mut reader).await? {
            Some(b) => b,
            None => return Ok(()),
        };

        let prompt = match Prompt::try_from(buf.trim()) {
            Ok(p) => p,
            Err(e) => {
                stdout
                    .write_all(format!("invalid prompt: {e}\n").as_bytes())
                    .await?;
                continue;
            }
        };

        let cancel = CancellationToken::new();
        let outcome = tokio::select! {
            r = agent.reply(session, prompt, cancel.clone()) => r,
            _ = tokio::signal::ctrl_c() => {
                cancel.cancel();
                stdout.write_all(b"\n^C cancelling...\n").await?;
                continue;
            }
        };

        match outcome {
            Ok(reply) => {
                stdout.write_all(b"\n").await?;
                stdout.write_all(reply.as_bytes()).await?;
                stdout.write_all(b"\n").await?;
            }
            Err(e) => {
                stdout
                    .write_all(format!("\nerror: {e}\n").as_bytes())
                    .await?;
            }
        }
    }
}

/// Reads lines until either:
/// * a blank line is entered after at least one non-blank line (returns `Some(buf)`), or
/// * EOF is reached (returns `Ok(None)`).
async fn read_until_blank_line(
    reader: &mut tokio::io::Lines<BufReader<tokio::io::Stdin>>,
) -> Result<Option<String>> {
    let mut buf = String::new();
    loop {
        match reader.next_line().await? {
            Some(line) if line.is_empty() && !buf.is_empty() => return Ok(Some(buf)),
            Some(line) if !line.is_empty() => {
                buf.push_str(&line);
                buf.push('\n');
            }
            Some(_blank_skip) => {}
            None => return Ok(None),
        }
    }
}
