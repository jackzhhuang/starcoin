// Copyright (c) Starcoin
// SPDX-License-Identifier: Apache-2.0

use crate::{account::Account, executor::FakeExecutor};
use starcoin_vm_runtime::starcoin_vm::StarcoinVM;
use starcoin_vm_types::on_chain_config::Version;

pub fn set_starcoin_version(executor: &mut FakeExecutor, version: Version) {
    let account =
        Account::new_genesis_account(starcoin_vm_types::on_chain_config::config_address());
    let txn = account
        .transaction()
        .payload(starcoin_stdlib::encode_version_set_version(version.major))
        .sequence_number(0)
        .sign();
    executor.new_block();
    executor.execute_and_apply(txn);

    //let new_vm = StarcoinVM::new(executor.get_state_view());
    let new_vm = StarcoinVM::new(None);
    assert_eq!(new_vm.internals().version().unwrap(), version);
}
