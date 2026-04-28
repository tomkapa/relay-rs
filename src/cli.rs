use std::io::{self, Write};

use anyhow::Result;
use tokio::io::{AsyncBufReadExt, BufReader};

use relay_rs::Agent;

pub async fn run(agent: &Agent) -> Result<()> {
    println!("relay-rs — type your prompt, blank line to send, Ctrl-D to exit.");

    let stdin = tokio::io::stdin();
    let mut reader = BufReader::new(stdin).lines();

    loop {
        print!("\n> ");
        io::stdout().flush().ok();

        let mut buf = String::new();
        loop {
            match reader.next_line().await? {
                Some(line) if line.is_empty() && !buf.is_empty() => break,
                Some(line) if line.is_empty() => continue,
                Some(line) => {
                    buf.push_str(&line);
                    buf.push('\n');
                }
                None => return Ok(()),
            }
        }

        let prompt = buf.trim();
        if prompt.is_empty() {
            continue;
        }

        match agent.reply(prompt).await {
            Ok(reply) => println!("\n{reply}"),
            Err(e) => eprintln!("\nerror: {e}"),
        }
    }
}
