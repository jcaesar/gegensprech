use anyhow::{Context, Result};
use itertools::Itertools;
use libpulse_binding::{
	sample::{Format, Spec},
	stream::Direction,
};
use libpulse_simple_binding::Simple;
use matrix_sdk::ruma::{events::room::message::AudioInfo, UInt};
use tokio::{
	sync::oneshot,
	task::{spawn_blocking, JoinHandle},
};

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
			const RATE: u32 = 16000;
			let spec = Spec {
				format: Format::S16NE,
				channels: 1,
				rate: RATE,
			};
			assert!(spec.is_valid());

			let input = Simple::new(
				None,
				env!("CARGO_PKG_NAME"),
				Direction::Record,
				None,
				"recording message",
				&spec,
				None,
				None,
			)
			.context("Pulseaudio open")?;
			let mut recorded = Vec::with_capacity(RATE as usize * 2);
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
			let data = ogg_opus::encode::<RATE, 1>(&recorded[..]).context("OGG Opus encode")?;
			let info = {
				let mut ai = AudioInfo::new();
				ai.duration = UInt::new(recorded.len() as u64 * 1000 / RATE as u64);
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
