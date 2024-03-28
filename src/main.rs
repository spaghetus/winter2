use clap::Parser;
use eframe::NativeOptions;
use std::path::PathBuf;

#[derive(Parser)]
pub struct Args {
	#[arg(long, env = "WINTER2_DB_LOCATION", default_value = "./.winter2db")]
	pub winter_db: PathBuf,
}

#[tokio::main]
async fn main() {
	let Args { winter_db } = Args::parse();
	let (gui, mut backend) = winter2::app::mk_app(winter_db.clone(), !winter_db.is_dir()).unwrap();
	tokio::spawn(async move { backend.work().await });
	eframe::run_native(
		"Winter2",
		NativeOptions::default(),
		Box::new(|_| Box::new(gui)),
	)
	.unwrap();
}
