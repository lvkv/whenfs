use std::{collections::HashSet, ffi::OsStr};

use fuser::{FileAttr, FileType};
use serde::{Deserialize, Serialize};

type Inode = u64;

#[derive(Clone, Serialize, Deserialize, Debug)]
pub enum FileSystemObject {
    File(FileObject),
    Dir(DirectoryObject),
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct FileObject {
    pub attr: FileAttr,
    pub name: String,
    pub data: Vec<u8>,
}

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct DirectoryObject {
    pub attr: FileAttr,
    pub entries: HashSet<DirectoryEntry>,
    pub name: String,
}

impl DirectoryObject {
    pub fn get_entry_by_name(&self, name: &OsStr) -> Option<&DirectoryEntry> {
        self.entries.iter().find(|&entry| *entry.name == *name)
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct DirectoryEntry {
    pub ino: Inode,
    pub file_type: FileType,
    pub name: String,
}

impl std::hash::Hash for DirectoryEntry {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.ino.hash(state);
    }
}

impl PartialEq for DirectoryEntry {
    fn eq(&self, other: &Self) -> bool {
        self.ino == other.ino
    }
}

impl Eq for DirectoryEntry {}

impl FileSystemObject {
    pub fn get_attr(&self) -> FileAttr {
        match self {
            FileSystemObject::File(f) => f.attr,
            FileSystemObject::Dir(d) => d.attr,
        }
    }

    pub fn mut_attr(&mut self) -> &mut FileAttr {
        match self {
            FileSystemObject::File(f) => &mut f.attr,
            FileSystemObject::Dir(d) => &mut d.attr,
        }
    }

    pub fn name(&self) -> &str {
        match self {
            FileSystemObject::File(f) => &f.name,
            FileSystemObject::Dir(d) => &d.name,
        }
    }
}

impl From<DirectoryObject> for FileSystemObject {
    fn from(value: DirectoryObject) -> Self {
        FileSystemObject::Dir(value)
    }
}

impl From<FileObject> for FileSystemObject {
    fn from(value: FileObject) -> Self {
        FileSystemObject::File(value)
    }
}
