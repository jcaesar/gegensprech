use std::{io::Cursor, sync::Mutex};

use anyhow::{Context, Result};
use itertools::Itertools;
use libpulse_binding::{
	sample::{Format, Spec},
	stream::Direction,
};
use libpulse_simple_binding::Simple;
use matrix_sdk::ruma::{events::room::message::AudioInfo, UInt};
use tokio::{
	sync::{mpsc, oneshot},
	task::{spawn_blocking, JoinHandle},
};

lazy_static::lazy_static! {
	static ref MUTEX: Mutex<()> = Mutex::new(());
}
const SAMPLE_RATE: u32 = 16000;

#[derive(Debug)]
pub struct Rec {
	pub data: Vec<u8>,
	pub info: AudioInfo,
}

pub struct RecProc {
	proc: JoinHandle<Result<Rec>>,
	done: oneshot::Sender<()>,
}

impl RecProc {
	pub fn start() -> Self {
		let (done, mut cont) = oneshot::channel::<()>();
		let proc = spawn_blocking(move || {
			let _guard = MUTEX.lock();
			let input = Simple::new(
				None,
				env!("CARGO_PKG_NAME"),
				Direction::Record,
				None,
				"recording message",
				&Spec {
					format: Format::S16NE,
					channels: 1,
					rate: SAMPLE_RATE,
				},
				None,
				None,
			)
			.context("Pulseaudio open")?;
			let mut recorded = Vec::with_capacity(SAMPLE_RATE as usize * 2);
			loop {
				let mut block = [0; 2048];
				input.read(&mut block)?;
				for (b1, b2) in block.iter().tuples() {
					recorded.push(i16::from_ne_bytes([*b1, *b2]))
				}
				match cont.try_recv() {
					Err(oneshot::error::TryRecvError::Empty) => (),
					Ok(()) => break,
					Err(e) => Err(e).context("aborted")?,
				}
			}
			let data =
				ogg_opus::encode::<SAMPLE_RATE, 1>(&recorded[..]).context("OGG Opus encode")?;
			let info = {
				let mut ai = AudioInfo::new();
				ai.duration = UInt::new(recorded.len() as u64 * 1000 / SAMPLE_RATE as u64);
				ai.mimetype = Some("media/ogg".to_owned());
				ai.size = UInt::new(data.len() as u64);
				ai
			};
			Ok(Rec { info, data })
		});
		RecProc { done, proc }
	}
	pub async fn finish(self) -> Result<Rec> {
		self.done.send(()).ok();
		Ok(self.proc.await??)
	}
}

pub async fn play(mut incoming: mpsc::Receiver<Vec<u8>>) -> Result<()> {
	loop {
		let data = incoming.recv().await;
		let data = match data {
			Some(data) => data,
			None => continue,
		};
		let proc = spawn_blocking(move || -> Result<_> {
			let _guard = MUTEX.lock();
			let (data, meta) =
				ogg_opus::decode::<_, 16000>(Cursor::new(data)).context("OGG Opus decode")?;
			let spec = Spec {
				format: Format::S16NE,
				channels: meta.channels as u8,
				rate: SAMPLE_RATE,
			};
			anyhow::ensure!(spec.is_valid(), "Playback spec invalid (weird channels?)");
			let output = Simple::new(
				None,
				env!("CARGO_PKG_NAME"),
				Direction::Playback,
				None,
				"playing message",
				&spec,
				None,
				None,
			)
			.context("Pulseaudio open")?;
			for chunk in data.chunks(2048) {
				let block = chunk
					.iter()
					.flat_map(|i| i.to_ne_bytes())
					.collect::<Vec<_>>();
				output.write(&block)?;
			}
			output.drain()?;
			Ok(())
		});
		if let Err(e) = proc.await?.context("play") {
			tracing::error!(?e, "Playback failed");
		}
	}
}
