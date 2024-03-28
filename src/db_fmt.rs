use base64::{engine::general_purpose::STANDARD, Engine};
use serde::{de::DeserializeOwned, Serialize};
use serde_json::Value;
use std::{fs::File, path::Path, string::FromUtf8Error};
use thiserror::Error;
use uuid::Uuid;

#[derive(Error, Debug)]
pub enum ValueToFsError {
	#[error("IO")]
	IO(#[from] std::io::Error),
	#[error("JSON")]
	JSON(#[from] serde_json::Error),
}

pub fn value_to_fs<S: Serialize>(path: &Path, value: &S) -> Result<(), ValueToFsError> {
	let value = serde_json::to_value(value)?;
	value_to_fs_inner(path, &value)
}

fn value_to_fs_inner(path: &Path, value: &Value) -> Result<(), ValueToFsError> {
	match value {
		// Exception for arrays of only numbers, since those might be byte arrays, which can be *very* long...
		Value::Array(array) if !array.iter().all(|el| matches!(el, Value::Number(_))) => {
			let orig_path = path;
			std::fs::create_dir_all(path)?;
			std::fs::read_dir(path)?
				.flatten()
				.map(|e| e.file_name().to_string_lossy().to_string())
				.filter(|name| name.starts_with("elem_"))
				.map(|name| path.join(name))
				.try_for_each(|path| {
					if path.is_dir() {
						if !path.starts_with(orig_path) {
							panic!()
						}
						std::fs::remove_dir_all(path)
					} else {
						std::fs::remove_file(path)
					}
				})?;
			std::fs::write(path.join(".type"), "array")?;
			for (n, item) in array.iter().enumerate() {
				let id = Uuid::new_v4();
				let path = path.join(format!("elem_{n}_{id}"));
				value_to_fs_inner(&path, item)?;
			}
		}
		Value::Object(object) => {
			let orig_path = path;
			std::fs::create_dir_all(path)?;
			std::fs::read_dir(path)?
				.flatten()
				.map(|e| e.file_name().to_string_lossy().to_string())
				.filter(|name| name.starts_with("key_"))
				.map(|name| path.join(name))
				.try_for_each(|path| {
					if path.is_dir() {
						if !path.starts_with(orig_path) {
							panic!()
						}
						std::fs::remove_dir_all(path)
					} else {
						std::fs::remove_file(path)
					}
				})?;
			std::fs::write(path.join(".type"), "dict")?;
			for (name, item) in object.iter() {
				let path = path.join(format!("key_{}", STANDARD.encode(name)));
				value_to_fs_inner(&path, item)?;
			}
		}
		other => {
			let mut file = std::fs::OpenOptions::new()
				.create(true)
				.truncate(true)
				.write(true)
				.open(path)?;
			serde_json::to_writer(&mut file, other)?;
		}
	}
	Ok(())
}

#[derive(Error, Debug)]
pub enum FsToValueError {
	#[error("IO")]
	IO(#[from] std::io::Error),
	#[error("JSON")]
	JSON(#[from] serde_json::Error),
	#[error("Bad directory type")]
	BadDirType(String),
	#[error("No directory type")]
	NoDirType,
	#[error("Bad Base64")]
	Base64(#[from] base64::DecodeError),
	#[error("Non-UTF8 name")]
	StringDecode(#[from] FromUtf8Error),
}

pub fn fs_to_value<D: DeserializeOwned>(path: &Path) -> Result<D, FsToValueError> {
	Ok(serde_json::from_value(fs_to_value_inner(path)?)?)
}

fn fs_to_value_inner(path: &Path) -> Result<Value, FsToValueError> {
	let stat = std::fs::metadata(path)?;
	let dir_type = std::fs::read_to_string(path.join(".type"));
	match (
		stat.file_type().is_dir(),
		dir_type.as_ref().map(|s| s.as_str()),
	) {
		(false, _) => Ok(serde_json::from_reader(File::open(path)?)?),
		(true, Ok("array")) => {
			let mut names: Vec<_> = std::fs::read_dir(path)?
				.flatten()
				.flat_map(|v| v.file_name().to_str().map(|s| s.to_string()))
				.filter(|s| s.starts_with("elem_"))
				.map(|s| (s[5..].to_string(), path.join(s)))
				.filter_map(|(index_and_name, path)| {
					let (index, name) = index_and_name.split_at(index_and_name.find('_')?);
					let name = &name[1..];
					Some((index.parse().ok()?, name.to_string(), path))
				})
				.collect();
			names.sort_by_key(|(index, _, _)| -> usize { *index });
			Ok(Value::Array(
				names
					.into_iter()
					.map(|(_, _, path)| fs_to_value(&path))
					.collect::<Result<_, _>>()?,
			))
		}
		(true, Ok("dict")) => {
			let names: Vec<_> = std::fs::read_dir(path)?
				.flatten()
				.flat_map(|v| v.file_name().to_str().map(|s| s.to_string()))
				.filter(|s| s.starts_with("key_"))
				.map(|orig_s| {
					STANDARD
						.decode(&orig_s[4..])
						.map_err(|e| -> FsToValueError { e.into() })
						.and_then(|b| String::from_utf8(b).map_err(|e| e.into()))
						.map(|s| (path.join(&orig_s), s))
				})
				.collect::<Result<_, _>>()?;
			Ok(Value::Object(
				names
					.into_iter()
					.map(|(path, name)| fs_to_value(&path).map(|value| (name, value)))
					.collect::<Result<_, _>>()?,
			))
		}
		(true, Ok(dir_type)) => Err(FsToValueError::BadDirType(dir_type.to_string())),
		(true, Err(_)) => Err(FsToValueError::NoDirType),
	}
}

#[cfg(test)]
mod test {
	use super::{fs_to_value, value_to_fs};
	use rss::Channel;
	use std::{io::BufReader, path::PathBuf};

	#[test]
	fn test_atom_feed() {
		let feed = reqwest::blocking::get("https://www.spreaker.com/show/4488937/episodes/feed")
			.unwrap()
			.bytes()
			.unwrap();
		let feed = Channel::read_from(BufReader::new(&feed[..])).unwrap();
		let path = PathBuf::from("./___test_ser_dir");
		value_to_fs(&path, &feed).unwrap();
		let read: Channel = fs_to_value(&path).unwrap();
		assert_eq!(feed.items.len(), read.items.len());
	}
}
