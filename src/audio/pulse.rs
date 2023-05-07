use crate::status;
use crate::status::AudioStatus;
use anyhow::{Context, Result};
use libpulse_binding::{
	sample::{Format, Spec},
	stream::Direction,
};
use libpulse_simple_binding::Simple;

use std::{io::Cursor, ops::ControlFlow};

pub(crate) const SAMPLE_RATE: u32 = 16000;

pub(crate) fn record(mut sample: impl FnMut(&[u8]) -> Result<ControlFlow<()>>) -> Result<()> {
	let input = Simple::new(
		None,
		env!("CARGO_PKG_NAME"),
		Direction::Record,
		None,
		"recording message",
		&Spec {
			format: Format::S16le,
			channels: 1,
			rate: SAMPLE_RATE,
		},
		None,
		None,
	)
	.context("Pulseaudio open")?;
	loop {
		let mut block = [0; 2048];
		input.read(&mut block)?;
		if sample(&block)?.is_break() {
			break;
		};
	}
	Ok(())
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
	let _guard = status::audio(AudioStatus::Playing);
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
