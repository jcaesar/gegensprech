#[cfg(not(feature = "audio-as-lib"))]
mod cmd;
#[cfg(feature = "audio-as-lib")]
mod pulse;

use std::sync::Mutex;

use anyhow::{Context, Result};

use matrix_sdk::ruma::events::room::message::AudioInfo;
use tokio::{
	sync::{mpsc, oneshot},
	task::{spawn_blocking, JoinHandle},
};

lazy_static::lazy_static! {
	static ref MUTEX: Mutex<()> = Mutex::new(());
}

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
		let (done, cont) = oneshot::channel::<()>();
		let proc = spawn_blocking(move || {
			let _guard = MUTEX.lock();
			#[cfg(feature = "audio-as-lib")]
			return pulse::record(cont);
			#[cfg(not(feature = "audio-as-lib"))]
			return cmd::record(cont);
		});
		RecProc { done, proc }
	}
	pub async fn finish(self) -> Result<Rec> {
		self.done.send(()).ok();
		Ok(self.proc.await??)
	}
}

pub async fn play(mut incoming: mpsc::Receiver<(Vec<u8>, oneshot::Sender<()>)>) -> Result<()> {
	loop {
		let data = incoming.recv().await;
		let data = match data {
			Some(data) => data,
			None => continue,
		};
		let (data, played) = data;
		let proc = spawn_blocking(move || -> Result<_> {
			let _guard = MUTEX.lock().unwrap();
			#[cfg(feature = "audio-as-lib")]
			pulse::play(data)?;
			#[cfg(not(feature = "audio-as-lib"))]
			cmd::play(data)?;
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
