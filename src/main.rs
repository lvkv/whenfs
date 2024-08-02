use std::path::PathBuf;

use calendar::{gcal::types::GCalEvent, CalendarClient};
use clap::Parser;
use fuser::MountOption;
use once_cell::sync::Lazy;
use store::CalStoreEntry;
use tracing::info;
use tracing_subscriber::{fmt, EnvFilter};

pub mod cache;
pub mod calendar;
pub mod fs;
pub mod object;
pub mod store;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    const FS_NAME: &str = "WhenFS";
    let _ = &*LOGGER;
    let args = Args::parse();
    let client = calendar::gcal::GCalClient::new(args.secret).await?;
    let calendar = match args.calendar {
        Some(calendar_id) => {
            info!("Attempting to use existing calendar");
            client.calendar_from_id(calendar_id).await?
        }
        None => {
            info!("Creating a new calendar");
            client
                .create_calendar(args.name.as_deref().unwrap_or(FS_NAME).into())
                .await?
        }
    };
    let store = store::CalStore::new(client, calendar);
    let cache = match args.root_event {
        Some(root_event_id) => {
            info!("Attempting to recover existing {FS_NAME} filesystem");
            let root_event = CalStoreEntry {
                name: String::from("root event"),
                events: vec![GCalEvent {
                    id: root_event_id,
                    details: Default::default(),
                }],
            };
            let cache = cache::WhenFSCache::recover(store, root_event).await?;
            info!("Recovered filesystem cache");
            cache
        }
        None => {
            info!("Creating a new filesystem");
            cache::WhenFSCache::new(store).await?
        }
    };

    let handle = tokio::runtime::Handle::current();
    let fs = fs::WhenFS::new(cache, handle)?;
    let mount_point = args.mount.as_deref().unwrap_or("/mnt/whenfs");
    info!("Mounting filesystem");
    fuser::mount2(fs, mount_point, &[MountOption::FSName(FS_NAME.into())])?;
    Ok(())
}

#[derive(Parser, Debug)]
struct Args {
    #[arg(long)]
    mount: Option<String>,
    #[arg(long)]
    secret: PathBuf,
    #[arg(long)]
    name: Option<String>,
    #[arg(long)]
    calendar: Option<String>,
    #[arg(long)]
    root_event: Option<String>,
}

static LOGGER: Lazy<()> = Lazy::new(|| {
    fmt::Subscriber::builder()
        .with_ansi(true)
        .with_target(true)
        .with_file(true)
        .with_line_number(true)
        .with_level(true)
        .with_env_filter(EnvFilter::from_default_env())
        .init();
    info!("Initializing logging...");
});
