use crate::audio::Rec;
use anyhow::{Context, Result};
use matrix_sdk::ruma::{events::room::message::AudioInfo, UInt};
use std::{
	io::{Read, Write},
	process::{Command, Stdio},
	thread,
	time::Instant,
};
use tokio::sync::oneshot;

static CLIENT_NAME_ARG: &'static str = concat!("--client-name=", env!("CARGO_PKG_NAME"));

fn read_pipe(mut pipe: impl Read + Send + 'static) -> oneshot::Receiver<Vec<u8>> {
	let (sender, receiver) = oneshot::channel();
	thread::spawn(move || {
		let mut buf = vec![];
		pipe.read_to_end(&mut buf).ok();
		sender.send(buf).ok();
	});
	receiver
}

#[tracing::instrument(skip(cont))]
pub(crate) fn record(cont: oneshot::Receiver<()>) -> Result<Rec> {
	let mut recorder = Command::new("pacat")
		.args([
			"--record",
			CLIENT_NAME_ARG,
			"--fix-format",
			"--fix-rate",
			"--fix-channels",
			"--file-format=ogg",
		])
		.stdin(Stdio::null())
		.stdout(Stdio::piped())
		.stderr(Stdio::piped())
		.spawn()
		.context("$ pacat --record")?;
	let start = Instant::now();
	let pid = recorder.id();
	let stdout = read_pipe(recorder.stdout.take().unwrap());
	let stderr = read_pipe(recorder.stderr.take().unwrap());
	cont.blocking_recv().ok();
	unsafe {
		libc::kill(pid as _, libc::SIGINT);
	}
	match recorder.wait() {
		Ok(ok) if ok.success() => {
			let data = stdout.blocking_recv().unwrap();
			let mut info: AudioInfo = AudioInfo::new();
			info.duration = UInt::new(Instant::now().duration_since(start).as_secs());
			info.mimetype = Some("media/ogg".to_owned());
			info.size = UInt::new(data.len() as u64);
			Ok(Rec { data, info })
		}
		Ok(_fail) => {
			let stderr = &stderr.blocking_recv().unwrap();
			let stderr = String::from_utf8_lossy(stderr);
			anyhow::bail!("{}", stderr)
		}
		Err(e) => Err(e).context("Process exit waiting failure"),
	}
}

#[tracing::instrument(skip(data))]
pub(crate) fn play(data: Vec<u8>) -> Result<()> {
	let mut player = Command::new("pacat")
		.args(["--playback", CLIENT_NAME_ARG])
		.stdin(Stdio::piped())
		.stdout(Stdio::piped())
		.stderr(Stdio::piped())
		.spawn()
		.context("$ pacat --playback")?;
	let stdout = read_pipe(player.stdout.take().unwrap());
	let stderr = read_pipe(player.stderr.take().unwrap());
	let mut stdin = player.stdin.take().unwrap();
	if stdin.write_all(&data).is_err() {
		player.kill().ok();
	};
	match player.wait() {
		Ok(status) if status.success() => Ok(()),
		Ok(_fail) => {
			let stdout = &stdout.blocking_recv().unwrap();
			let stdout = String::from_utf8_lossy(stdout);
			let stderr = &stderr.blocking_recv().unwrap();
			let stderr = String::from_utf8_lossy(stderr);
			let msg = match (stdout.is_empty(), stderr.is_empty()) {
				(false, false) => format!("Msg:\n{}Err:\n{}", stdout, stderr).into(),
				(true, false) => stdout,
				(false, true) => stderr,
				(true, true) => "(silent failure)".into(),
			};
			anyhow::bail!("{}", msg)
		}
		Err(e) => Err(e).context("Process exit waiting failure"),
	}
}
