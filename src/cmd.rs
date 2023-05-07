use anyhow::{bail, Context, Result};
use serde::Deserialize;
use signal_child::Signalable;
use std::{
	borrow::Cow,
	collections::HashMap,
	fmt::Debug,
	fs::read,
	path::Path,
	process::{self, Child, Stdio},
	time::Duration,
};
use tokio::{sync::mpsc::Sender, task::JoinHandle, time::sleep};
use tracing::{debug, error, warn};

use crate::audio;

static MORSE_CMDS: &str = "cmds.yaml";

#[derive(Deserialize, Debug, Hash, PartialEq, Eq)]
pub enum Morse {
	Long,
	Short,
}

impl TryFrom<char> for Morse {
	type Error = anyhow::Error;
	fn try_from(value: char) -> Result<Self, Self::Error> {
		Ok(match value {
			'_' | '-' | '—' | 'ー' => Morse::Long,
			'.' | '°' | '·' | '・' => Morse::Short,
			c => bail!("Expected . or -, got {}", c),
		})
	}
}

#[derive(Deserialize, PartialEq, Eq, Hash)]
#[serde(try_from = "Cow<str>")]
pub struct MorseWord(pub Vec<Morse>);

impl TryFrom<Cow<'_, str>> for MorseWord {
	type Error = anyhow::Error;
	fn try_from(value: Cow<str>) -> Result<Self, Self::Error> {
		Ok(MorseWord(
			value
				.chars()
				.map(|c| c.try_into())
				.collect::<Result<Vec<Morse>>>()?,
		))
	}
}

impl Debug for MorseWord {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_tuple("MorseWord")
			.field(&format_args!(
				"{}",
				self.0
					.iter()
					.map(|c| match c {
						Morse::Long => '—',
						Morse::Short => '·',
					})
					.collect::<String>(),
			))
			.finish()
	}
}

type Cmds = HashMap<MorseWord, Command>;

#[derive(Deserialize, Debug)]
enum Command {
	SubProcess {
		cmd: Vec<String>,
	},
	LoopTape {
		#[serde(deserialize_with = "deser_humantime")]
		time: Duration,
		#[serde(default = "ftrue")]
		play: bool,
		#[serde(default)]
		send: bool,
	},
}

fn ftrue() -> bool {
	true
}

fn deser_humantime<'de, D: serde::Deserializer<'de>>(de: D) -> Result<Duration, D::Error> {
	Ok(String::deserialize(de)?
		.parse::<humantime::Duration>()
		.map_err(serde::de::Error::custom)?
		.into())
}

pub enum Running {
	SubProcess { inner: Option<Child> },
	Play { task: JoinHandle<Result<()>> },
}

impl Running {
	pub async fn terminate(mut self) {
		match &mut self {
			Running::Play { task } => {
				// In general, playback should be abortable
				// But that's also for voice messages, so TODO
				task.await.ok();
			}
			Running::SubProcess { inner } => {
				if let Some(child) = inner.take() {
					terminate_child(child).await;
				}
			}
		}
	}
}

impl Drop for Running {
	fn drop(&mut self) {
		match self {
			Running::Play { .. } => (), // whatev
			Running::SubProcess { inner } => {
				if let Some(child) = inner.take() {
					tokio::spawn(terminate_child(child));
				}
			}
		}
	}
}

async fn terminate_child(mut child: Child) {
	child.interrupt().ok();
	for _ in 0..10 {
		sleep(Duration::from_millis(300)).await;
		match child.try_wait() {
			Ok(Some(_)) => return,
			Ok(None) => (),
			Err(err) => {
				warn!(?err, "Child termination error");
				return;
			}
		}
	}
	warn!("Child didn't exit after 3 seconds, killing…");
	child.kill().ok();
	warn!(status = ?child.wait(), "Child killed");
}

pub struct ButtonCommands {
	cmds: Cmds,
	tape: Option<audio::LoopTape>,
}

impl ButtonCommands {
	#[tracing::instrument]
	pub fn load(cfg_dir: &Path) -> Result<ButtonCommands> {
		let file = &cfg_dir.join(MORSE_CMDS);
		let file = match file.exists() {
			true => read(file).context("Open cmd file")?,
			false => {
				return Ok(ButtonCommands {
					cmds: HashMap::new(),
					tape: None,
				})
			}
		};
		let cmds = serde_yaml::from_slice::<Cmds>(&file).context("Parse cmd file")?;
		debug!(?cmds);
		let tape = cmds
			.iter()
			.filter_map(|cmd| match cmd {
				(_, Command::LoopTape { time, .. }) => Some(*time),
				_ => None,
			})
			.max()
			.map(audio::LoopTape::start);
		Ok(ButtonCommands { cmds, tape })
	}

	pub(crate) fn exec(&self, cmd: MorseWord, messages: &Sender<audio::Rec>) -> Option<Running> {
		self.cmds.get(&cmd).and_then(|cmd| match &cmd {
			Command::SubProcess { cmd } => match &cmd[..] {
				[] => unreachable!(),
				[path, args @ ..] => Some(Running::SubProcess {
					inner: process::Command::new(path)
						.args(args)
						.stdin(Stdio::null())
						.spawn()
						.ok(),
				}),
			},
			Command::LoopTape { time, play, send } => {
				let samp = self
					.tape
					.as_ref()
					.expect("Initialized to illegal state")
					.get(*time);
				debug!(?time, ?play, ?send, samp_len = samp.len(), "LoopTape use");
				if *send {
					match audio::encode_raw(&samp) {
						Ok(enc) => {
							messages.try_send(enc).ok();
						}
						Err(e) => error!("Failed to encode accumulated recording: {}", e),
					}
				}
				if *play {
					let task = tokio::task::spawn_blocking(move || audio::play_raw(&samp, 1));
					Some(Running::Play { task })
				} else {
					None
				}
			}
		})
	}
}
