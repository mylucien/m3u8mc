//! m3u8mcctl：通过 Unix Domain Socket 与 m3u8mcd 通信的小型 CLI 工具，
//! 供 init 脚本（reload/stop-all）和 LuCI 后端（status --json）调用。

use clap::{Parser, Subcommand};
use m3u8mc_common::protocol::{Request, Response};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

#[derive(Parser, Debug)]
#[command(name = "m3u8mcctl")]
struct Args {
    #[arg(long, default_value = "/var/run/m3u8mcd.sock")]
    socket: String,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// 查看 daemon 与各频道运行状态
    Status {
        #[arg(long)]
        json: bool,
    },
    /// 通知 daemon 重新加载配置（增量 diff，未变更频道不受影响）
    Reload,
    /// 频道相关操作
    Channel {
        #[command(subcommand)]
        action: ChannelAction,
    },
    /// 停止所有频道（init 脚本 stop 时调用）
    StopAll,
}

#[derive(Subcommand, Debug)]
enum ChannelAction {
    Start {
        #[arg(long)]
        name: String,
    },
    Stop {
        #[arg(long)]
        name: String,
    },
    Restart {
        #[arg(long)]
        name: String,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    match args.command {
        Command::Status { json } => {
            let resp = send(&args.socket, Request::Status).await?;
            match resp {
                Response::Status(status) => {
                    if json {
                        println!("{}", serde_json::to_string_pretty(&status)?);
                    } else {
                        print_status_human(&status);
                    }
                }
                Response::Error { message } => {
                    eprintln!("error: {message}");
                    std::process::exit(1);
                }
                Response::Ok => {}
            }
        }
        Command::Reload => {
            expect_ok(send(&args.socket, Request::Reload).await?)?;
            println!("reload ok");
        }
        Command::StopAll => {
            // 停止 daemon 内所有正在运行的频道任务（ffmpeg 进程等），
            // daemon 进程本身继续存活，channels map 清空；
            // 后续可通过 reload 或 channel start 重新拉起。
            expect_ok(send(&args.socket, Request::StopAll).await?)?;
            println!("stop-all ok: all running channels have been stopped");
        }
        Command::Channel { action } => {
            let req = match action {
                ChannelAction::Start { name } => Request::ChannelStart { name },
                ChannelAction::Stop { name } => Request::ChannelStop { name },
                ChannelAction::Restart { name } => Request::ChannelRestart { name },
            };
            expect_ok(send(&args.socket, req).await?)?;
            println!("ok");
        }
    }

    Ok(())
}

async fn send(socket_path: &str, req: Request) -> anyhow::Result<Response> {
    let stream = UnixStream::connect(socket_path)
        .await
        .map_err(|e| anyhow::anyhow!("failed to connect to {socket_path}: {e} (daemon not running?)"))?;
    let (read_half, mut write_half) = stream.into_split();
    let mut payload = serde_json::to_string(&req)?;
    payload.push('\n');
    write_half.write_all(payload.as_bytes()).await?;

    let mut lines = BufReader::new(read_half).lines();
    let line = lines
        .next_line()
        .await?
        .ok_or_else(|| anyhow::anyhow!("daemon closed connection without responding"))?;
    Ok(serde_json::from_str(&line)?)
}

fn expect_ok(resp: Response) -> anyhow::Result<()> {
    match resp {
        Response::Ok => Ok(()),
        Response::Error { message } => anyhow::bail!(message),
        Response::Status(_) => Ok(()),
    }
}

fn print_status_human(status: &m3u8mc_common::protocol::DaemonStatus) {
    println!("uptime: {}s", status.uptime_secs);
    println!(
        "{:<12} {:<8} {:<7} {:<10} {:<6} {:<8} {:<8} {}",
        "CHANNEL", "ENABLED", "RUNNING", "MODE", "CACHE", "RESTARTS", "OVERLOAD", "PLAY_URL"
    );
    for ch in &status.channels {
        println!(
            "{:<12} {:<8} {:<7} {:<10} {:<6} {:<8} {:<8} {}",
            ch.id, ch.enabled, ch.running, ch.mode, ch.cached_segments, ch.ffmpeg_restarts, ch.overloaded, ch.play_url
        );
    }
}
