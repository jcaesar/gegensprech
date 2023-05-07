use crate::status;
use crate::status::AudioStatus;
use anyhow::{Context, Result};
use signal_child::Signalable;
use std::{
	borrow::Cow,
	io::{Read, Write},
	mem,
	ops::ControlFlow,
	os::unix::process::ExitStatusExt,
	process::{Command, Stdio},
	thread,
};
use tokio::sync::oneshot;
use tracing::{debug, warn};

static CLIENT_NAME_ARG: &str = concat!("--client-name=", env!("CARGO_PKG_NAME"));

fn read_pipe(mut pipe: impl Read + Send + 'static) -> oneshot::Receiver<Vec<u8>> {
	let (sender, receiver) = oneshot::channel();
	thread::spawn(move || {
		let mut buf = vec![];
		pipe.read_to_end(&mut buf).ok();
		sender.send(buf).ok();
	});
	receiver
}

pub(crate) const SAMPLE_RATE: u32 = 48000;

#[tracing::instrument(skip(sample))]
pub(crate) fn record(mut sample: impl FnMut(&[u8]) -> Result<ControlFlow<()>>) -> Result<()> {
	let mut recorder = Command::new("pacat")
		.args([
			"--record",
			CLIENT_NAME_ARG,
			"--raw",
			"--format=s16le",
			"--channels=1",
			format!("--rate={}", SAMPLE_RATE).as_str(),
			"--latency-msec=50",
		])
		.stdin(Stdio::null())
		.stdout(Stdio::piped())
		.stderr(Stdio::piped())
		.spawn()
		.context("$ pacat --record")?;
	let mut stdout = recorder.stdout.take().unwrap();
	let stderr = read_pipe(recorder.stderr.take().unwrap());
	loop {
		let mut block = [0; 2048];
		stdout.read_exact(&mut block)?;
		if sample(&block)?.is_break() {
			break;
			// TODO: losing some samples in stdout here...
		};
	}
	recorder.interrupt().context("Stop subcommand")?;
	let exited = recorder.wait().context("Process exit waiting failure")?;
	debug!(
		?exited,
		success = exited.success(),
		code = exited.code(),
		signal = exited.signal(),
		"pacat exited"
	);
	if exited.success() || exited.signal() == Some(1) {
		Ok(())
	} else {
		let stderr = &stderr.blocking_recv().unwrap();
		let stderr = String::from_utf8_lossy(stderr);
		let msg = match exited.code() {
			Some(code) => Cow::Owned(format!("pacat exited with code {}", code)),
			None => "pacat exited".into(),
		};
		anyhow::bail!("{}: {}", msg, stderr)
	}
}

#[tracing::instrument(skip(data))]
pub(crate) fn play(data: &[i16], channels: u16) -> Result<()> {
	let data = data
		.into_iter()
		.flat_map(|s| s.to_le_bytes())
		.collect::<Vec<_>>();
	// Since we're already shelling out, why not shell out to ffmpeg if the ogg decode fails...
	// Another day.
	let mut player = Command::new("pacat")
		.args([
			"--playback",
			CLIENT_NAME_ARG,
			"--raw",
			"--format=s16le",
			"--rate=48000",
			format!("--channels={}", channels).as_str(),
		])
		.stdin(Stdio::piped())
		.stdout(Stdio::piped())
		.stderr(Stdio::piped())
		.spawn()
		.context("$ pacat --playback")?;
	let stdout = read_pipe(player.stdout.take().unwrap());
	let stderr = read_pipe(player.stderr.take().unwrap());
	let mut stdin = player.stdin.take().unwrap();
	let _guard = status::audio(AudioStatus::Playing);
	let written = stdin.write_all(&data);
	if written.is_err() {
		warn!(?written, "pipe write error, kill pacat");
		player.kill().ok();
	};
	mem::drop(stdin);
	let exited = player.wait().context("Process exit waiting failure")?;
	let stdout = &stdout.blocking_recv().unwrap();
	let stdout = String::from_utf8_lossy(stdout);
	let stderr = &stderr.blocking_recv().unwrap();
	let stderr = String::from_utf8_lossy(stderr);
	debug!(
		?exited,
		?stdout,
		?stderr,
		?written,
		input_len = data.len(),
		success = exited.success(),
		code = exited.code(),
		signal = exited.signal(),
		"pacat exited"
	);
	written.context("Write to pacat")?;
	if !exited.success() {
		let msg = match (stdout.is_empty(), stderr.is_empty()) {
			(false, false) => format!("Msg:\n{}Err:\n{}", stdout, stderr).into(),
			(true, false) => stderr,
			(false, true) => stdout,
			(true, true) => "(silent failure)".into(),
		};
		anyhow::bail!("{}", msg);
	}
	Ok(())
}
