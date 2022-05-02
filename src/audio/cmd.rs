use crate::audio::Rec;
use crate::status;
use crate::status::AudioStatus;
use anyhow::{Context, Result};
use itertools::Itertools;
use matrix_sdk::ruma::{events::room::message::AudioInfo, UInt};
use signal_child::Signalable;
use std::{
	borrow::Cow,
	io::{Cursor, Read, Write},
	mem,
	os::unix::process::ExitStatusExt,
	process::{Command, Stdio},
	thread,
	time::Instant,
};
use tokio::sync::oneshot;
use tracing::{debug, warn};

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

fn record_read(mut pipe: impl Read + Send + 'static) -> oneshot::Receiver<Result<Vec<u8>>> {
	let (sender, receiver) = oneshot::channel();
	thread::spawn(move || {
		sender
			.send((|| {
				let mut buf = vec![0; 48];
				pipe.read_exact(&mut buf[..])?;
				let _guard = status::audio(AudioStatus::Recording);
				pipe.read_to_end(&mut buf)?;
				Ok(buf)
			})())
			.ok();
	});
	receiver
}

#[tracing::instrument(skip(cont))]
pub(crate) fn record(cont: oneshot::Receiver<()>) -> Result<Rec> {
	let mut recorder = Command::new("pacat")
		.args([
			"--record",
			CLIENT_NAME_ARG,
			"--raw",
			"--format=s16le",
			"--channels=1",
			"--rate=48000",
			"--latency-msec=50",
		])
		.stdin(Stdio::null())
		.stdout(Stdio::piped())
		.stderr(Stdio::piped())
		.spawn()
		.context("$ pacat --record")?;
	let start = Instant::now();
	let stdout = record_read(recorder.stdout.take().unwrap());
	let stderr = read_pipe(recorder.stderr.take().unwrap());
	cont.blocking_recv().ok();
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
		let data = stdout
			.blocking_recv()
			.unwrap()
			.context("read pacat --record sdtout")?;
		let data = data
			.into_iter()
			.tuples()
			.map(|(a, b)| i16::from_le_bytes([a, b]))
			.collect::<Vec<_>>();
		// Pulseaudio and pacat have a startup time. If the button is released before that is done…
		// TODO: Make sure this doesn't cause an exit with error
		anyhow::ensure!(data.len() > 500, "Short recording");
		let data = ogg_opus::encode::<48000, 1>(&data).context("OGG Opus encode")?;
		let mut info: AudioInfo = AudioInfo::new();
		info.duration = UInt::new(Instant::now().duration_since(start).as_secs());
		info.mimetype = Some("media/ogg".to_owned());
		info.size = UInt::new(data.len() as u64);
		Ok(Rec { data, info })
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
pub(crate) fn play(data: Vec<u8>, mtyp: Option<String>) -> Result<()> {
	let (data, meta) =
		ogg_opus::decode::<_, 48000>(Cursor::new(data)).context("OGG Opus decode")?;
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
		])
		.arg(format!("--channels={}", meta.channels))
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
