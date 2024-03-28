use crate::db_fmt::{fs_to_value, value_to_fs, FsToValueError};
use eframe::egui::{CentralPanel, CollapsingHeader, ScrollArea, SidePanel, TopBottomPanel, Vec2b};
use egui_notify::{Toast, ToastLevel, Toasts};
use rss::{Channel, Guid};
use serde::{Deserialize, Serialize};
use std::{
	collections::HashMap,
	convert::Infallible,
	io::BufReader,
	ops::Mul,
	path::PathBuf,
	sync::{
		atomic::{AtomicUsize, Ordering},
		Arc, OnceLock,
	},
};
use tokio::{
	sync::mpsc::{Receiver, Sender},
	task::JoinHandle,
};

type Mutation =
	Box<dyn FnOnce(&mut Db, &Sender<(ToastLevel, String)>) -> eyre::Result<()> + Send + Sync>;

pub fn mk_app(path: PathBuf, init: bool) -> Result<(Gui, Backend), FsToValueError> {
	let db = if init {
		let db = Db::default();
		value_to_fs(&path, &db).unwrap();
		db
	} else {
		fs_to_value(&path)?
	};
	let (send_mutations, recv_mutations) = tokio::sync::mpsc::channel(1024);
	let (send_db, recv_db) = tokio::sync::mpsc::channel(1024);
	let (send_toast, recv_toast) = tokio::sync::mpsc::channel(1024);
	let queued = Arc::new(AtomicUsize::new(0));
	let db = Arc::new(db);
	Ok((
		Gui {
			mutations: send_mutations,
			new_state: recv_db,
			queued: queued.clone(),
			db: db.clone(),
			playing: None,
			staged_feed: None,
			selected_feed: None,
			jobs: vec![],
			send_toast: send_toast.clone(),
			recv_toast,
			toasts: Toasts::new(),
		},
		Backend {
			mutations: recv_mutations,
			new_db: send_db,
			queued,
			path,
			db,
			toast: send_toast,
		},
	))
}

pub struct Gui {
	mutations: Sender<Mutation>,
	new_state: Receiver<Arc<Db>>,
	recv_toast: Receiver<(ToastLevel, String)>,
	send_toast: Sender<(ToastLevel, String)>,
	queued: Arc<AtomicUsize>,
	db: Arc<Db>,
	playing: Option<JoinHandle<()>>,
	jobs: Vec<JoinHandle<()>>,
	#[allow(clippy::type_complexity)]
	staged_feed: Option<(String, JoinHandle<()>, Arc<OnceLock<eyre::Result<Feed>>>)>,
	selected_feed: Option<(String, Option<Guid>)>,
	toasts: Toasts,
}

#[derive(Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Db {
	pub feeds: HashMap<String, Feed>,
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct Feed {
	pub feed: Channel,
	/// Table mapping articles to the read fraction. Media articles might be partially read.
	pub read_articles: HashMap<String, f64>,
}

impl Gui {
	fn send_mutation(&self, mutation: Mutation) {
		let mutations = self.mutations.clone();
		tokio::spawn(async move {
			mutations.send(mutation).await.unwrap();
		});
	}

	fn status_line(&mut self, ctx: &eframe::egui::Context) {
		TopBottomPanel::top("status").show(ctx, |ui| {
			ui.horizontal(|ui| {
				ui.label(format!(
					"M: {}; J: {}",
					self.queued.load(Ordering::Relaxed),
					self.jobs.len()
				));
				ui.separator();
				if ui.button("New Feed").clicked() {
					self.staged_feed =
						Some((String::new(), tokio::spawn(async {}), Default::default()));
				}
				if ui.button("Refresh").clicked() {
					self.refresh();
				}
				if let Some(jh) = &self.playing {
					if ui.button("STOP").clicked() {
						jh.abort();
						self.playing = None;
					}
				}
			});
		});
	}

	fn refresh(&mut self) {
		for (url, feed) in self.db.feeds.iter() {
			let url = url.clone();
			let read_articles = feed.read_articles.clone();
			let send_toast = self.send_toast.clone();
			let send_mutation = self.mutations.clone();
			self.jobs.push(tokio::spawn(async move {
				let response = match reqwest::get(&url).await {
					Err(e) => {
						send_toast
							.send((
								ToastLevel::Error,
								format!("Downloading feed {url} failed with {e}"),
							))
							.await
							.unwrap();
						return;
					}
					Ok(v) => v,
				};
				let bytes = match response.bytes().await {
					Err(e) => {
						send_toast
							.send((
								ToastLevel::Error,
								format!("Reading feed {url} failed with {e}"),
							))
							.await
							.unwrap();
						return;
					}
					Ok(v) => v,
				};
				let feed = match rss::Channel::read_from(&bytes[..]) {
					Err(e) => {
						send_toast
							.send((
								ToastLevel::Error,
								format!("Parsing feed {url} failed with {e}"),
							))
							.await
							.unwrap();
						return;
					}
					Ok(v) => v,
				};
				let feed = Feed {
					feed,
					read_articles,
				};
				send_mutation
					.send(Box::new(move |db, _| {
						db.feeds.insert(url, feed);
						Ok(())
					}))
					.await
					.unwrap();
			}))
		}
	}

	fn new_feed_editor(&mut self, ctx: &eframe::egui::Context) {
		if let Some((url, jh, info)) = &mut self.staged_feed {
			let mut clear_feed = false;
			let mut commit = false;
			SidePanel::left("new_feed").show(ctx, |ui| {
				ui.heading("New Feed");
				if ui.text_edit_singleline(url).changed() {
					jh.abort();
					*info = Default::default();
					*jh = tokio::spawn({
						let url = url.clone();
						let info = info.clone();
						async move {
							let response = match reqwest::get(url).await {
								Ok(v) => v,
								Err(e) => {
									info.get_or_init(move || Err(e.into()));
									return;
								}
							};
							let bytes = match response.bytes().await {
								Ok(v) => v,
								Err(e) => {
									info.get_or_init(move || Err(e.into()));
									return;
								}
							};
							let channel = match Channel::read_from(BufReader::new(&bytes[..])) {
								Ok(v) => v,
								Err(e) => {
									info.get_or_init(move || Err(e.into()));
									return;
								}
							};
							info.get_or_init(move || {
								Ok(Feed {
									feed: channel,
									read_articles: HashMap::default(),
								})
							});
						}
					});
				}
				if let Some(feed) = info.get() {
					match feed {
						Ok(f) => {
							ui.label(format!(
								"Feed {} retrieved OK, {} articles.",
								f.feed.title,
								f.feed.items.len()
							));
							commit = ui.button("Commit").clicked();
						}
						Err(e) => {
							ui.label(format!("Feed NOT OK, {e}"));
						}
					}
				} else {
					ui.spinner();
				}
				clear_feed = ui.button("Cancel").clicked();
			});
			if commit {
				let url = url.clone();
				let Some(Ok(feed)) = info.get() else {
					unreachable!()
				};
				let feed = feed.clone();
				self.send_mutation(Box::new(move |state, _| {
					state.feeds.insert(url, feed);
					Ok(())
				}));
			}
			if clear_feed || commit {
				self.staged_feed = None;
			}
		}
	}

	fn feed_picker(&mut self, ui: &mut eframe::egui::Ui) {
		for (url, feed) in self.db.feeds.iter() {
			ui.horizontal(|ui| {
				ui.heading(&feed.feed.title);
				let total = feed.feed.items.len();
				let completed = feed
					.feed
					.items
					.iter()
					.filter(|i| {
						feed.read_articles
							.get(i.guid().map(|g| g.value()).unwrap_or("???"))
							.copied()
							.unwrap_or(0.0) >= 1.0
					})
					.count();
				ui.label(format!("{completed}/{total}"));
				if ui.button(">").clicked() {
					self.selected_feed = Some((url.clone(), None));
				}
			});
			CollapsingHeader::new("Description")
				.id_source(url)
				.show(ui, |ui| {
					ui.label(feed.feed.description());
				});
			ui.separator();
		}
	}
}

impl eframe::App for Gui {
	fn update(&mut self, ctx: &eframe::egui::Context, _frame: &mut eframe::Frame) {
		while let Ok(new_db) = self.new_state.try_recv() {
			self.db = new_db;
			ctx.request_repaint();
		}
		while let Ok((level, message)) = self.recv_toast.try_recv() {
			self.toasts.add(Toast::custom(message, level));
		}
		if self.queued.load(Ordering::Relaxed) > 0 || !self.jobs.is_empty() {
			ctx.request_repaint();
		}
		self.jobs.retain(|network| !network.is_finished());
		self.status_line(ctx);
		self.new_feed_editor(ctx);
		CentralPanel::default().show(ctx, |ui| {
			if self.selected_feed.is_some() && ui.button("< Select feed").clicked() {
				self.selected_feed = None;
			}
			ScrollArea::vertical()
				.auto_shrink(Vec2b::new(false, false))
				.show(ui, |ui| {
					let send_mutation = self.mutations.clone();
					if let Some((feed_url, feed, selected_article)) =
						self.selected_feed.as_mut().and_then(|(feed_url, art)| {
							self.db
								.feeds
								.get(feed_url)
								.map(|feed| (feed_url.clone(), feed, art))
						}) {
						if selected_article.is_some() && ui.button("< Select article").clicked() {
							*selected_article = None;
						}
						if let Some(article) = selected_article
							.as_ref()
							.and_then(|art| feed.feed.items.iter().find(|a| a.guid() == Some(art)))
						{
						} else {
							for article in &feed.feed.items {
								let guid = article.guid().map(|g| g.value()).unwrap_or("???");
								let completion = feed
									.read_articles
									.get(guid)
									.copied()
									.unwrap_or(0.0)
									.clamp(0.0, 1.0)
									.mul(100.0)
									.round();
								ui.horizontal(|ui| {
									ui.heading(article.title().unwrap_or("???"));
									ui.label(format!("{completion}%"));
									if ui.button(">").clicked() {
										*selected_article = article.guid.clone();
									}
									if ui
										.button(if completion > 0.0 { "x" } else { "r" })
										.clicked()
									{
										let guid = guid.to_string();
										let feed_url = feed_url.clone();
										let send_mutation = send_mutation.clone();
										tokio::spawn(async move {
											send_mutation
												.clone()
												.send(Box::new(move |db, _| {
													if let Some(feed) =
														db.feeds.get_mut(feed_url.as_str())
													{
														feed.read_articles.insert(
															guid,
															if completion > 0.0 {
																0.0
															} else {
																1.0
															},
														);
													}
													Ok(())
												}))
												.await
										});
									}
								});
								if let Some(desc) = article.description() {
									CollapsingHeader::new("Description").id_source(guid).show(
										ui,
										|ui| {
											ui.label(desc);
										},
									);
								}
							}
						}
						if let Some(article_id) = selected_article.clone() {
							if ui.button("< Done").clicked() {
								*selected_article = None;
								self.send_mutation(Box::new(move |db, _| {
									if let Some(feed) = db.feeds.get_mut(feed_url.as_str()) {
										feed.read_articles.insert(article_id.value, 1.0);
									}
									Ok(())
								}));
							}
						}
					} else {
						self.feed_picker(ui);
					}
				});
		});
	}
}

pub struct Backend {
	mutations: Receiver<Mutation>,
	queued: Arc<AtomicUsize>,
	new_db: Sender<Arc<Db>>,
	toast: Sender<(ToastLevel, String)>,
	path: PathBuf,
	db: Arc<Db>,
}

impl Backend {
	pub async fn work(&mut self) -> eyre::Result<Infallible> {
		loop {
			let mut mutations = vec![];
			self.mutations.recv_many(&mut mutations, 128).await;
			self.queued.fetch_add(mutations.len(), Ordering::Relaxed);
			let mut new_db: Db = tokio::task::spawn_blocking({
				let path = self.path.clone();
				move || fs_to_value(&path)
			})
			.await??;
			for mutation in mutations {
				mutation(&mut new_db, &self.toast)?;
				self.queued.fetch_sub(1, Ordering::Relaxed);
			}
			tokio::task::spawn_blocking({
				let path = self.path.clone();
				let new_db = new_db.clone();
				move || value_to_fs(&path, &new_db)
			})
			.await??;
			if new_db == *self.db {
				continue;
			}
			self.db = Arc::new(new_db);
			self.new_db.send(self.db.clone()).await?;
		}
	}
}
