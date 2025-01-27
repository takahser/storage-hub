use std::collections::HashMap;

use common::types::HashT;
use sp_core::H256;
use storage_hub_infra::types::{Chunk, ChunkId, FileProof, Key, Leaf, Metadata};

use sp_trie::{recorder::Recorder, MemoryDB, Trie, TrieDBBuilder, TrieLayout, TrieMut};
use trie_db::TrieDBMutBuilder;

use crate::traits::{FileStorage, FileStorageError, FileStorageWriteStatus};

pub struct FileData<T: TrieLayout + 'static> {
    root: HashT<T>,
    memdb: MemoryDB<T::Hash>,
}

impl<T: TrieLayout + 'static> FileData<T> {
    fn new() -> Self {
        Self {
            root: Default::default(),
            memdb: MemoryDB::default(),
        }
    }

    pub fn get_root(&self) -> H256 {
        H256::from_slice(
            self.root
                .as_ref()
                .try_into()
                .expect("trie hash should be 32 bytes"),
        )
    }

    pub fn stored_chunks_count(&self) -> u64 {
        let trie = TrieDBBuilder::<T>::new(&self.memdb, &self.root).build();
        let stored_chunks = trie.key_iter().iter().count() as u64;
        stored_chunks
    }
}

pub struct InMemoryFileStorage<T: TrieLayout + 'static> {
    pub metadata: HashMap<Key, Metadata>,
    pub file_data: HashMap<Key, FileData<T>>,
}

impl<T: TrieLayout> InMemoryFileStorage<T> {
    pub fn new() -> Self {
        Self {
            metadata: HashMap::new(),
            file_data: HashMap::new(),
        }
    }
}

impl<T: TrieLayout + 'static> FileStorage for InMemoryFileStorage<T> {
    fn generate_proof(
        &self,
        file_key: &Key,
        chunk_id: &ChunkId,
    ) -> Result<FileProof, FileStorageError> {
        let metadata = self
            .metadata
            .get(file_key)
            .ok_or(FileStorageError::FileDoesNotExist)?;

        let file_data = self.file_data.get(file_key).expect(
            format!(
                "Invariant broken! Metadata for file key {:?} found but no associated trie",
                file_key
            )
            .as_str(),
        );

        if metadata.chunk_count() != file_data.stored_chunks_count() {
            return Err(FileStorageError::IncompleteFile);
        }

        if file_data.get_root() != metadata.fingerprint {
            return Err(FileStorageError::FingerprintAndStoredFileMismatch);
        }

        let recorder: Recorder<T::Hash> = Recorder::default();

        // A `TrieRecorder` is needed to create a proof of the "visited" leafs, by the end of this process.
        let mut trie_recorder = recorder.as_trie_recorder(file_data.root);

        let trie = TrieDBBuilder::<T>::new(&file_data.memdb, &file_data.root)
            .with_recorder(&mut trie_recorder)
            .build();

        let chunk: Option<Vec<u8>> = trie
            .get(&chunk_id.to_be_bytes())
            .map_err(|_| FileStorageError::FailedToGetFileChunk)?;

        let chunk = chunk.ok_or(FileStorageError::FileChunkDoesNotExist)?;

        // Drop the `trie_recorder` to release the `recorder`
        drop(trie_recorder);

        // Generate proof
        let proof = recorder
            .drain_storage_proof()
            .to_compact_proof::<T::Hash>(file_data.root)
            .map_err(|_| FileStorageError::FailedToGenerateCompactProof)?;

        Ok(FileProof {
            proven: Leaf {
                key: (*chunk_id).clone(),
                data: chunk,
            },
            proof,
            root: file_data.get_root(),
        })
    }

    fn delete_file(&mut self, file_key: &Key) {
        self.metadata.remove(file_key);
        self.file_data.remove(file_key);
    }

    fn get_metadata(&self, file_key: &Key) -> Result<Metadata, FileStorageError> {
        self.metadata
            .get(file_key)
            .cloned()
            .ok_or(FileStorageError::FileDoesNotExist)
    }

    fn set_metadata(&mut self, file_key: Key, metadata: Metadata) {
        self.metadata.insert(file_key, metadata);
        self.file_data.insert(file_key, FileData::new());
    }

    fn get_chunk(&self, file_key: &Key, chunk_id: &ChunkId) -> Result<Chunk, FileStorageError> {
        let file_data = self.file_data.get(file_key);
        let file_data = file_data.ok_or(FileStorageError::FileDoesNotExist)?;

        let trie = TrieDBBuilder::<T>::new(&file_data.memdb, &file_data.root).build();

        Ok(trie
            .get(&chunk_id.to_be_bytes())
            .map_err(|_| FileStorageError::FailedToGetFileChunk)?
            .ok_or(FileStorageError::FileChunkDoesNotExist)?)
    }

    fn write_chunk(
        &mut self,
        file_key: &Key,
        chunk_id: &ChunkId,
        data: &Chunk,
    ) -> Result<FileStorageWriteStatus, FileStorageError> {
        let file_data = self
            .file_data
            .get_mut(file_key)
            .ok_or(FileStorageError::FileDoesNotExist)?;

        let mut trie =
            TrieDBMutBuilder::<T>::new(&mut file_data.memdb, &mut file_data.root).build();

        // Check that we don't have a chunk already stored.
        if trie
            .contains(&chunk_id.to_be_bytes())
            .map_err(|_| FileStorageError::FailedToGetFileChunk)?
        {
            return Err(FileStorageError::FileChunkAlreadyExists);
        }

        // Insert the chunk into the file trie.
        trie.insert(&chunk_id.to_be_bytes(), &data)
            .map_err(|_| FileStorageError::FailedToInsertFileChunk)?;

        drop(trie);

        let metadata = self.metadata.get(file_key).expect(
            format!(
            "Invariant broken! Metadata for file key {:?} not found but associated trie is present",
            file_key
        )
            .as_str(),
        );

        // Check if we have all the chunks for the file.
        if metadata.chunk_count() != file_data.stored_chunks_count() {
            return Ok(FileStorageWriteStatus::FileIncomplete);
        }

        // If we have all the chunks, check if the file metadata fingerprint and the file trie
        // root matches.
        if file_data.get_root() != metadata.fingerprint {
            return Err(FileStorageError::FingerprintAndStoredFileMismatch);
        }

        Ok(FileStorageWriteStatus::FileComplete)
    }
}
