//! Real process and pipe fixture for isolated tuning replays.

use anyhow::{Context, Result, bail, ensure};
use loom_driver::git::{commit_paths_in, read_remote_names, sync_head_commit_sha};
use serde_json::{Value, json};
use std::{
    env,
    fs::{self, OpenOptions},
    io::{self, BufRead, Read, Write},
    net::TcpStream,
    path::Path,
    process::{Command, ExitCode, Stdio},
};

#[derive(Clone, Copy, Eq, PartialEq)]
enum Mode {
    Regress,
    Hang,
    Fail,
    Complete,
}
impl TryFrom<&str> for Mode {
    type Error = anyhow::Error;
    fn try_from(value: &str) -> Result<Self> {
        match value {
            "regress" => Ok(Self::Regress),
            "hang" => Ok(Self::Hang),
            "fail" => Ok(Self::Fail),
            "complete" => Ok(Self::Complete),
            _ => bail!("unknown tune replay mode: {value}"),
        }
    }
}

fn emit(value: &Value) -> Result<()> {
    let mut output = io::stdout().lock();
    serde_json::to_writer(&mut output, value)?;
    writeln!(output)?;
    output.flush()?;
    Ok(())
}
fn input() -> Result<Value> {
    let mut line = String::new();
    io::stdin().lock().read_line(&mut line)?;
    Ok(serde_json::from_str(&line)?)
}
pub fn hold(record: &Path) -> Result<()> {
    let endpoint = env::var("LOOM_TEST_REPLAY_ENDPOINT")?;
    let mut peer = TcpStream::connect(endpoint)?;
    writeln!(peer, "{}", std::process::id())?;
    let mut buffer = [0; 32];
    if peer.read(&mut buffer)? > 0 {
        fs::write(
            format!("{}.escaped", record.display()),
            "process survived cancellation",
        )?;
        peer.write_all(b"escaped\n")?;
    }
    Ok(())
}
pub fn run(arguments: &[String]) -> Result<ExitCode> {
    ensure!(
        arguments.len() >= 2,
        "tune replay requires record, mode and spawn arguments"
    );
    let record = Path::new(&arguments[0]);
    let mode = Mode::try_from(arguments[1].as_str())?;
    let index = arguments
        .iter()
        .position(|a| a == "--spawn-config")
        .context("missing replay spawn config")?;
    let config: Value = serde_json::from_slice(&fs::read(
        arguments
            .get(index + 1)
            .context("missing spawn config path")?,
    )?)?;
    let workspace = Path::new(
        config["workspace"]
            .as_str()
            .context("missing fixture workspace")?,
    );
    ensure!(
        workspace.is_absolute()
            && Path::new(
                config["extensions"]["loom"]["scratch_dir"]
                    .as_str()
                    .context("missing scratch directory")?
            )
            .starts_with(workspace),
        "tune replay workspace/scratch escaped fixture"
    );
    ensure!(
        config.get("mounts").is_none_or(|v| v == &json!([])),
        "tune replay inherited extra mounts"
    );
    ensure!(
        fs::read_to_string(workspace.join("src/lib.rs"))? == "original fixture\n"
            && fs::read(workspace.join("binary"))? == [0, 255, 0],
        "fixture source was not freshly isolated"
    );
    ensure!(
        !workspace.join("other-side").exists()
            && !workspace.join(".beads").exists()
            && read_remote_names(workspace)?.is_empty(),
        "replay inherited private state or remotes"
    );
    let head = sync_head_commit_sha(workspace)?.to_string();
    writeln!(io::stderr().lock(), "[wrix] Starting container (mock)...")?;
    let probe = input()?;
    ensure!(probe["type"] == "get_state", "missing startup probe");
    emit(
        &json!({"type":"response","id":probe["id"],"command":"get_state","success":true,"data":{"isStreaming":false,"isCompacting":false,"messageCount":0,"pendingMessageCount":0}}),
    )?;
    let mut prompt = input()?;
    if prompt["type"] == "set_thinking_level" {
        ensure!(
            prompt["level"] == config["extensions"]["loom"]["thinking_level"],
            "replay thinking level differs from spawn contract"
        );
        emit(
            &json!({"type":"response","id":prompt["id"],"command":"set_thinking_level","success":true}),
        )?;
        prompt = input()?;
    }
    let message = prompt["message"]
        .as_str()
        .context("missing replay prompt")?;
    ensure!(
        prompt["type"] == "prompt"
            && message.contains("fixture task context")
            && !message.contains("original fixture"),
        "replay prompt leaked fixture source or omitted task context"
    );
    fs::write(workspace.join("src/lib.rs"), "mutated\n")?;
    fs::write(workspace.join("other-side"), "private\n")?;
    if mode == Mode::Regress && message.contains("Tuned Guidance") {
        fs::write(workspace.join("forbidden"), "unreported unrelated edit\n")?;
        commit_paths_in(
            workspace,
            &["src/lib.rs", "other-side", "forbidden"],
            "hide changes from status",
        )?;
    }
    let mut entry = json!({"workspace":workspace,"head":head.trim(),"pid":std::process::id()});
    let mut child = if mode == Mode::Hang {
        let child = Command::new(env::current_exe()?)
            .arg("--tune-replay-hold")
            .arg(record)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .spawn()?;
        entry["child"] = json!(child.id());
        Some(child)
    } else {
        None
    };
    let mut log = OpenOptions::new().create(true).append(true).open(record)?;
    serde_json::to_writer(&mut log, &entry)?;
    writeln!(log)?;
    drop(log);
    match mode {
        Mode::Hang => {
            let hold_result = hold(record);
            if let Some(child) = &mut child {
                ensure!(child.wait()?.success(), "replay child failed");
            }
            hold_result?;
        }
        Mode::Fail => return Ok(ExitCode::from(17)),
        Mode::Complete | Mode::Regress => {
            emit(
                &json!({"type":"message_update","assistantMessageEvent":{"type":"text_delta","text":"src/lib.rs\nLOOM_COMPLETE"}}),
            )?;
            emit(&json!({"type":"agent_end","messages":[]}))?;
        }
    }
    Ok(ExitCode::SUCCESS)
}
