#[cfg(not(feature = "audio-as-lib"))]
mod cmd;
#[cfg(feature = "audio-as-lib")]
mod pulse;
use anyhow::{Context, Result};
use itertools::Itertools;
use matrix_sdk::ruma::{events::room::message::AudioInfo, UInt};
use once_cell::sync::Lazy;
use std::{
	ops::ControlFlow,
	sync::{Arc, Mutex},
	time::Duration,
};
use tokio::{
	sync::{mpsc, oneshot},
	task::{spawn_blocking, JoinHandle},
};

use crate::status::{self, AudioStatus};

static MUTEX: Lazy<Mutex<()>> = Lazy::new(|| Mutex::new(()));

pub struct Rec {
	pub data: Vec<u8>,
	pub info: AudioInfo,
}

impl std::fmt::Debug for Rec {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("Rec")
			.field("data", &format!("[u8; {}]", self.data.len()))
			.field("info", &self.info)
			.finish()
	}
}

pub struct RecProc {
	proc: JoinHandle<Result<Rec>>,
	done: oneshot::Sender<()>,
}

impl RecProc {
	#[tracing::instrument]
	pub fn start() -> Self {
		let (done, cont) = oneshot::channel::<()>();
		let proc = spawn_blocking(move || {
			let _guard = MUTEX.lock();
			#[cfg(not(feature = "audio-as-lib"))]
			use cmd::SAMPLE_RATE;
			#[cfg(feature = "audio-as-lib")]
			use pulse::SAMPLE_RATE;
			let mut recorded = Vec::with_capacity(SAMPLE_RATE as usize * 2);
			let mut led_guard = None;
			let mut sample = |block: &[u8]| {
				for (b1, b2) in block.iter().tuples() {
					recorded.push(i16::from_le_bytes([*b1, *b2]))
				}
				match cont.try_recv() {
					Err(oneshot::error::TryRecvError::Empty) => {
						led_guard.get_or_insert_with(|| status::audio(AudioStatus::Recording));
						Ok(ControlFlow::Continue(()))
					}
					Ok(()) => Ok(ControlFlow::Break(())),
					Err(e) => Err(e).context("aborted"),
				}
			};
			#[cfg(feature = "audio-as-lib")]
			pulse::record(sample)?;
			#[cfg(not(feature = "audio-as-lib"))]
			cmd::record(sample)?;
			let data =
				ogg_opus::encode::<SAMPLE_RATE, 1>(&recorded[..]).context("OGG Opus encode")?;
			let info = {
				let mut ai = AudioInfo::new();
				ai.duration = Some(Duration::from_secs_f64(
					recorded.len() as f64 / SAMPLE_RATE as f64,
				));
				ai.mimetype = Some("media/ogg".to_owned());
				ai.size = UInt::new(data.len() as u64);
				ai
			};
			Ok(Rec { info, data })
		});
		RecProc { done, proc }
	}
	#[tracing::instrument(skip(self))]
	pub async fn finish(self) -> Result<Rec> {
		self.done.send(()).ok();
		self.proc
			.await
			.context("Recording spawn error")?
			.context("Recording error")
	}
}

#[tracing::instrument(skip(incoming, background_cmd))]
pub async fn play(
	mut incoming: mpsc::Receiver<(Vec<u8>, Option<String>, oneshot::Sender<()>)>,
	background_cmd: Arc<Mutex<Option<crate::cmd::Running>>>,
) -> Result<()> {
	loop {
		let background_cmd = background_cmd.clone();
		let data = incoming.recv().await;
		let data = match data {
			Some(data) => data,
			None => continue,
		};
		let (data, mtyp, played) = data;
		let proc = spawn_blocking(move || -> Result<_> {
			let mut background_cmd = background_cmd.lock().unwrap();
			if let Some(background_cmd) = background_cmd.take() {
				background_cmd.terminate();
			}
			let _guard = MUTEX.lock().unwrap();
			#[cfg(feature = "audio-as-lib")]
			pulse::play(data, mtyp)?;
			#[cfg(not(feature = "audio-as-lib"))]
			cmd::play(data, mtyp)?;
			Ok(())
		});
		match proc.await?.context("play") {
			Ok(()) => {
				// Unused variable bug?
				played.send(()).ok();
			}
			Err(e) => tracing::error!(?e, "Playback failed"),
		}
	}
}
