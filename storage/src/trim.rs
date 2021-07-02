// Copyright (C) 2019-2021 Aleo Systems Inc.
// This file is part of the snarkOS library.

// The snarkOS library is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

// The snarkOS library is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the
// GNU General Public License for more details.

// You should have received a copy of the GNU General Public License
// along with the snarkOS library. If not, see <https://www.gnu.org/licenses/>.

use crate::{
    Ledger,
    COL_BLOCK_HEADER,
    COL_BLOCK_LOCATOR,
    COL_BLOCK_TRANSACTIONS,
    COL_CHILD_HASHES,
    COL_TRANSACTION_LOCATION,
};
use snarkvm_algorithms::traits::LoadableMerkleParameters;
use snarkvm_dpc::{BlockHeader, BlockHeaderHash, DatabaseTransaction, Op, Storage, StorageError, TransactionScheme};
use snarkvm_utilities::FromBytes;

use parking_lot::Mutex;
use rayon::prelude::*;
use tracing::*;

use std::collections::HashSet;

impl<T: TransactionScheme + Send + Sync, P: LoadableMerkleParameters, S: Storage + Sync> Ledger<T, P, S> {
    pub fn trim(&self) -> Result<(), StorageError> {
        info!("Checking for obsolete objects in the storage...");

        let locator_col = self.storage.get_col(COL_BLOCK_LOCATOR)?;
        let canon_hashes = locator_col
            .into_iter()
            .filter(|(locator_key, locator_value)| locator_key.len() < locator_value.len())
            .map(|(_block_number_bytes, block_hash)| block_hash)
            .collect::<HashSet<_>>();

        let headers_col = self.storage.get_col(COL_BLOCK_HEADER)?;

        let database_transaction = Mutex::new(DatabaseTransaction::new());

        headers_col
            .into_par_iter()
            .try_for_each::<_, Result<(), StorageError>>(|(block_hash_bytes, block_header_bytes)| {
                if !canon_hashes.contains(&block_hash_bytes) {
                    let block_hash = BlockHeaderHash::new(block_hash_bytes.to_vec());
                    let block_header = BlockHeader::read(&block_header_bytes[..])?;

                    trace!("Block {} is obsolete, staging its objects for removal", block_hash);

                    // Remove obsolete transactions

                    database_transaction.lock().push(Op::Delete {
                        col: COL_BLOCK_TRANSACTIONS,
                        key: block_hash_bytes.to_vec(),
                    });
                    for transaction in self.get_block_transactions(&block_hash)?.0 {
                        database_transaction.lock().push(Op::Delete {
                            col: COL_TRANSACTION_LOCATION,
                            key: transaction.transaction_id()?.to_vec(),
                        });
                    }

                    // Remove parent's obsolete reference

                    let parent_hash = &block_header.previous_block_hash;
                    let mut parent_child_hashes = self.get_child_block_hashes(parent_hash)?;

                    if let Some(index) = parent_child_hashes
                        .iter()
                        .position(|child_hash| *child_hash == block_hash)
                    {
                        parent_child_hashes.remove(index);

                        database_transaction.lock().push(Op::Insert {
                            col: COL_CHILD_HASHES,
                            key: parent_hash.0.to_vec(),
                            value: bincode::serialize(&parent_child_hashes)?,
                        });
                    }

                    // Remove the obsolete header

                    database_transaction.lock().push(Op::Delete {
                        col: COL_BLOCK_HEADER,
                        key: block_hash_bytes.into_vec(),
                    });
                }

                Ok(())
            })?;

        let database_transaction = database_transaction.into_inner();

        let num_items = database_transaction.0.len();
        if !database_transaction.0.is_empty() {
            self.storage.batch(database_transaction)?;
        }
        info!("The storage was trimmed successfully ({} items removed)!", num_items);

        Ok(())
    }
}
