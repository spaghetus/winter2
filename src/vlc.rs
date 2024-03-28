use std::{num::ParseFloatError, process::Stdio, time::Duration};

use thiserror::Error;
use tokio::{
	io::{AsyncReadExt, AsyncWriteExt},
	process::{Child, Command},
};

pub struct Vlc {
	child: Child,
}

#[derive(Debug, Error)]
pub enum VlcError {
	#[error("IO error")]
	IO(#[from] tokio::io::Error),
	#[error("Malformed output")]
	API(String),
	#[error("Bad float")]
	BadFloat(#[from] ParseFloatError),
}

impl Vlc {
	pub async fn new(url: &str) -> Result<Self, VlcError> {
		let child = Command::new("vlc")
			.stdin(Stdio::piped())
			.stdout(Stdio::piped())
			.arg("--extraintf")
			.arg("lua")
			.arg(url)
			.kill_on_drop(true)
			.spawn()?;
		let mut vlc = Self { child };
		let out = vlc.child.stdout.as_mut().unwrap();
		while let Ok(r) = out.read_u8().await {
			if r == b'>' {
				break;
			}
		}

		Ok(vlc)
	}
	pub async fn cmd(&mut self, cmd: &str) -> Result<String, VlcError> {
		self.child
			.stdin
			.as_mut()
			.unwrap()
			.write_all(format!("{cmd}\n").as_bytes())
			.await?;
		let out = self.child.stdout.as_mut().unwrap();
		let mut output = Vec::new();
		while let Ok(read) = out.read_u8().await {
			if read == b'>' {
				break;
			}
			output.push(read)
		}
		Ok(String::from_utf8_lossy(&output).trim().to_string())
	}

	pub async fn is_playing(&mut self) -> Result<bool, VlcError> {
		match self.cmd("is_playing").await?.as_str() {
			"0" => Ok(false),
			"1" => Ok(true),
			malformed => Err(VlcError::API(malformed.to_string())),
		}
	}

	pub async fn wait_for_playing(&mut self) -> Result<(), VlcError> {
		while !self.is_playing().await? {
			tokio::time::sleep(Duration::from_millis(100)).await;
		}
		Ok(())
	}

	pub async fn play_time(&mut self) -> Result<f64, VlcError> {
		Ok(self.cmd("get_time").await?.parse()?)
	}

	pub async fn video_length(&mut self) -> Result<f64, VlcError> {
		Ok(self.cmd("get_length").await?.parse()?)
	}

	pub async fn progress(&mut self) -> Result<f64, VlcError> {
		Ok(self.play_time().await? / self.video_length().await?)
	}
}

#[cfg(test)]
mod tests {
	use crate::vlc::Vlc;

	#[tokio::test]
	async fn vlc_works_ok() {
		let mut vlc = Vlc::new("").await.unwrap();
		let output = vlc.is_playing().await.unwrap();
		assert!(!output);
	}
}
