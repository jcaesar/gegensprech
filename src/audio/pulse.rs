use super::Rec;
use anyhow::{Context, Result};
use itertools::Itertools;
use libpulse_binding::{
	sample::{Format, Spec},
	stream::Direction,
};
use libpulse_simple_binding::Simple;
use matrix_sdk::ruma::{events::room::message::AudioInfo, UInt};
use std::io::Cursor;
use tokio::sync::oneshot;

const SAMPLE_RATE: u32 = 16000;

pub(crate) fn record(mut cont: oneshot::Receiver<()>) -> Result<Rec> {
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
	let data = ogg_opus::encode::<SAMPLE_RATE, 1>(&recorded[..]).context("OGG Opus encode")?;
	let info = {
		let mut ai = AudioInfo::new();
		ai.duration = UInt::new(recorded.len() as u64 * 1000 / SAMPLE_RATE as u64);
		ai.mimetype = Some("media/ogg".to_owned());
		ai.size = UInt::new(data.len() as u64);
		ai
	};
	Ok(Rec { info, data })
}

pub(crate) fn play(data: Vec<u8>, mtyp: Option<String>) -> Result<()> {
	let (data, meta) = ogg_opus::decode::<_, 16000>(Cursor::new(data)).context(format!(
		"Decode {} as OGG Opus",
		mtyp.as_deref().unwrap_or("MIME unknown")
	))?;
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
}
