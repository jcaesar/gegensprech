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
	thread::{self, sleep},
	time::Duration,
};
use tracing::{debug, warn};

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
	SubProcess { cmd: Vec<String> },
}

pub struct Running {
	inner: Option<Child>,
}

impl Running {
	pub fn terminate(mut self) {
		if let Some(child) = self.inner.take() {
			terminate_child(child);
		}
	}
}

impl Drop for Running {
	fn drop(&mut self) {
		if let Some(child) = self.inner.take() {
			thread::spawn(move || terminate_child(child));
		}
	}
}

fn terminate_child(mut child: Child) {
	child.interrupt().ok();
	for _ in 0..10 {
		sleep(Duration::from_millis(300));
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
				})
			}
		};
		let cmds = serde_yaml::from_slice::<Cmds>(&file).context("Parse cmd file")?;
		debug!(?cmds);
		Ok(ButtonCommands { cmds })
	}

	pub(crate) fn exec(&self, cmd: MorseWord) -> Option<Running> {
		self.cmds.get(&cmd).and_then(|cmd| match &cmd {
			Command::SubProcess { cmd } => match &cmd[..] {
				[] => unreachable!(),
				[path, args @ ..] => Some(Running {
					inner: process::Command::new(path)
						.args(args)
						.stdin(Stdio::null())
						.spawn()
						.ok(),
				}),
			},
		})
	}
}
