use crate::store::Store;
use crate::{object::FileSystemObject, store::RecoveryDetails};
use async_trait::async_trait;
use dashmap::DashMap;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc, RwLock,
};
use tracing::{debug, info};

pub type Inode = u64;
pub type CachedWhenFSObject = Arc<RwLock<FileSystemObject>>;

#[async_trait(?Send)]
pub trait Cache {
    type Error: Send + Sync + std::fmt::Debug + std::error::Error;

    async fn get(&self, ino: Inode) -> Result<Option<CachedWhenFSObject>, Self::Error>;

    async fn insert(&mut self, ino: Inode, item: FileSystemObject) -> Result<Inode, Self::Error>;

    fn new_inode(&self) -> Inode;

    fn get_recovery_id(&self) -> RecoveryDetails;
}

#[derive(Debug)]
pub struct WhenFSCache<TStore: Store> {
    ino_to_id: DashMap<Inode, TStore::Entry>,
    id_to_obj: DashMap<TStore::Entry, CachedWhenFSObject>,
    inode_count: AtomicU64,
    store: TStore,
    root_event: TStore::Entry,
}

impl<TStore: Store> WhenFSCache<TStore> {
    pub async fn new(store: TStore) -> Result<Self, <Self as Cache>::Error> {
        let ino_to_id = DashMap::new();
        let root_event = store.store(&ino_to_id, "root event".to_string()).await?;
        let this = Self {
            inode_count: AtomicU64::new(fuser::FUSE_ROOT_ID + 1),
            ino_to_id,
            id_to_obj: DashMap::new(),
            store,
            root_event,
        };

        Ok(this)
    }

    pub async fn recover(
        store: TStore,
        root_id: TStore::Entry,
    ) -> Result<Self, <Self as Cache>::Error> {
        debug!("Attempting cache recovery");
        let ino_to_id: DashMap<u64, TStore::Entry> = store.retrieve(root_id.clone()).await?;
        debug!("Recovered inode mapping");
        let inode_count = ino_to_id
            .iter()
            .map(|entry| *entry.key())
            .max()
            .expect("Couldn't find inode count")
            + 1;
        info!("Recovered filesystem cache");
        Ok(Self {
            ino_to_id,
            id_to_obj: DashMap::new(),
            inode_count: inode_count.into(),
            store,
            root_event: root_id,
        })
    }
}

#[async_trait(?Send)]
impl<TStore: Store> Cache for WhenFSCache<TStore> {
    type Error = TStore::Error;

    async fn get(&self, ino: Inode) -> Result<Option<CachedWhenFSObject>, TStore::Error> {
        if let Some(id) = self.ino_to_id.get(&ino) {
            let cached = match self.id_to_obj.get(&id) {
                Some(cached) => Arc::clone(&cached),
                None => {
                    let retrieved = Arc::new(RwLock::new(self.store.retrieve(id.clone()).await?));
                    self.id_to_obj.insert(id.clone(), retrieved.clone());
                    retrieved
                }
            };
            Ok(Some(cached))
        } else {
            Ok(None)
        }
    }

    async fn insert(&mut self, ino: Inode, item: FileSystemObject) -> Result<Inode, TStore::Error> {
        let id = self.store.store(&item, item.name().to_string()).await?;
        self.ino_to_id.insert(ino, id.clone());
        self.id_to_obj.insert(id, Arc::new(RwLock::new(item)));
        let new_block = self
            .store
            .update(self.root_event.clone(), &self.ino_to_id)
            .await?;
        self.root_event = new_block;
        Ok(ino)
    }

    fn new_inode(&self) -> Inode {
        self.inode_count.fetch_add(1, Ordering::SeqCst)
    }

    fn get_recovery_id(&self) -> RecoveryDetails {
        self.store.get_raw_id(&self.root_event)
    }
}

pub trait BlockingCache
where
    Self: Cache,
{
    type Error: std::fmt::Debug + std::error::Error;

    // async fn recover<TStore: Store>(store: TStore, root_event_id: TStore::Id) -> Self;

    fn get_blocking(
        &self,
        ino: Inode,
    ) -> Result<Option<CachedWhenFSObject>, <Self as Cache>::Error>;

    fn insert_blocking(
        &mut self,
        ino: Inode,
        item: FileSystemObject,
    ) -> Result<Inode, <Self as Cache>::Error>;
}

impl<TStore: Store> BlockingCache for WhenFSCache<TStore> {
    type Error = <Self as Cache>::Error;

    fn get_blocking(
        &self,
        ino: Inode,
    ) -> Result<Option<CachedWhenFSObject>, <Self as Cache>::Error> {
        info!(%ino, "Handling request for inode");
        let handle = tokio::runtime::Handle::current();
        let _guard = handle.enter();
        futures::executor::block_on(self.get(ino))
    }

    fn insert_blocking(
        &mut self,
        ino: Inode,
        item: FileSystemObject,
    ) -> Result<Inode, <Self as Cache>::Error> {
        let handle = tokio::runtime::Handle::current();
        let _guard = handle.enter();
        futures::executor::block_on(self.insert(ino, item))
    }
}
