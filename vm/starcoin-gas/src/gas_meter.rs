// Copyright (c) The Starcoin Core Contributors
// SPDX-License-Identifier: Apache-2.0

//! This module contains the official gas meter implementation, along with some top-level gas
//! parameters and traits to help manipulate them.

use gas_algebra_ext::{
    FromOnChainGasSchedule, Gas, InitialGasSchedule, MiscGasParameters, ToOnChainGasSchedule,
};
use move_binary_format::errors::{Location, PartialVMError, PartialVMResult, VMResult};
use move_core_types::gas_algebra::{AbstractMemorySize, NumArgs};
use move_core_types::language_storage::ModuleId;
use move_core_types::{
    gas_algebra::{InternalGas, NumBytes},
    vm_status::StatusCode,
};
use move_vm_types::gas::{GasMeter, SimpleInstruction};
use move_vm_types::views::{TypeView, ValueView};
use starcoin_logger::prelude::*;
use std::collections::BTreeMap;

// Change log:
// - V3
//   - Add memory quota
//   - Storage charges:
//     - Distinguish between new and existing resources
//     - One item write comes with 1K free bytes
//     - abort with STORATGE_WRITE_LIMIT_REACHED if WriteOps or Events are too large
// - V2
//   - Table
//     - Fix the gas formula for loading resources so that they are consistent with other
//       global operations.
// - V1
//   - TBA

use gas_algebra_ext::InstructionGasParameters;
use gas_algebra_ext::TransactionGasParameters;

/// Gas parameters for all native functions.
#[derive(Debug, Clone)]
pub struct NativeGasParameters {
    pub move_stdlib: move_stdlib::natives::GasParameters,
    pub nursery: move_stdlib::natives::NurseryGasParameters,
    pub starcoin_natives: starcoin_natives::GasParameters,
    pub table: move_table_extension::GasParameters,
}

impl FromOnChainGasSchedule for NativeGasParameters {
    fn from_on_chain_gas_schedule(gas_schedule: &BTreeMap<String, u64>) -> Option<Self> {
        Some(Self {
            move_stdlib: FromOnChainGasSchedule::from_on_chain_gas_schedule(gas_schedule)?,
            nursery: FromOnChainGasSchedule::from_on_chain_gas_schedule(gas_schedule)?,
            starcoin_natives: FromOnChainGasSchedule::from_on_chain_gas_schedule(gas_schedule)?,
            table: FromOnChainGasSchedule::from_on_chain_gas_schedule(gas_schedule)?,
        })
    }
}

impl ToOnChainGasSchedule for NativeGasParameters {
    fn to_on_chain_gas_schedule(&self) -> Vec<(String, u64)> {
        let mut entries = self.move_stdlib.to_on_chain_gas_schedule();
        entries.extend(self.nursery.to_on_chain_gas_schedule());
        entries.extend(self.starcoin_natives.to_on_chain_gas_schedule());
        entries.extend(self.table.to_on_chain_gas_schedule());
        entries
    }
}

impl NativeGasParameters {
    pub fn zeros() -> Self {
        Self {
            move_stdlib: move_stdlib::natives::GasParameters::zeros(),
            nursery: move_stdlib::natives::NurseryGasParameters::zeros(),
            starcoin_natives: starcoin_natives::GasParameters::zeros(),
            table: move_table_extension::GasParameters::zeros(),
        }
    }
}

impl InitialGasSchedule for NativeGasParameters {
    fn initial() -> Self {
        Self {
            move_stdlib: InitialGasSchedule::initial(),
            nursery: InitialGasSchedule::initial(),
            starcoin_natives: InitialGasSchedule::initial(),
            table: InitialGasSchedule::initial(),
        }
    }
}

/// Gas parameters for everything that is needed to run the Starcoin blockchain, including
/// instructions, transactions and native functions from various packages.
#[derive(Debug, Clone)]
pub struct StarcoinGasParameters {
    pub misc: MiscGasParameters,
    pub instr: InstructionGasParameters,
    pub txn: TransactionGasParameters,
    pub natives: NativeGasParameters,
}

impl FromOnChainGasSchedule for StarcoinGasParameters {
    fn from_on_chain_gas_schedule(gas_schedule: &BTreeMap<String, u64>) -> Option<Self> {
        Some(Self {
            misc: FromOnChainGasSchedule::from_on_chain_gas_schedule(gas_schedule)?,
            natives: FromOnChainGasSchedule::from_on_chain_gas_schedule(gas_schedule)?,
            instr: FromOnChainGasSchedule::from_on_chain_gas_schedule(gas_schedule)?,
            txn: FromOnChainGasSchedule::from_on_chain_gas_schedule(gas_schedule)?,
        })
    }
}

impl ToOnChainGasSchedule for StarcoinGasParameters {
    fn to_on_chain_gas_schedule(&self) -> Vec<(String, u64)> {
        let mut entries = self.instr.to_on_chain_gas_schedule();
        entries.extend(self.txn.to_on_chain_gas_schedule());
        entries.extend(self.natives.to_on_chain_gas_schedule());
        entries
    }
}

impl StarcoinGasParameters {
    // Only used for genesis and for tests where we need a cost table and
    // don't have a genesis storage state.
    pub fn zeros() -> Self {
        Self {
            misc: MiscGasParameters::zeros(),
            instr: InstructionGasParameters::zeros(),
            txn: TransactionGasParameters::zeros(),
            natives: NativeGasParameters::zeros(),
        }
    }
}

impl InitialGasSchedule for StarcoinGasParameters {
    fn initial() -> Self {
        Self {
            misc: InitialGasSchedule::initial(),
            instr: InitialGasSchedule::initial(),
            txn: InitialGasSchedule::initial(),
            natives: InitialGasSchedule::initial(),
        }
    }
}

/// The official gas meter used inside the Starcoin VM.
/// It maintains an internal gas counter, measured in internal gas units, and carries an environment
/// consisting all the gas parameters, which it can lookup when performing gas calculations.
pub struct StarcoinGasMeter {
    gas_params: StarcoinGasParameters,
    balance: InternalGas,
    charge: bool,
}

impl StarcoinGasMeter {
    pub fn new(gas_params: StarcoinGasParameters, balance: impl Into<Gas>) -> Self {
        let balance = balance.into().to_unit_with_params(&gas_params.txn);
        Self {
            gas_params,
            balance,
            charge: true,
        }
    }

    pub fn balance(&self) -> Gas {
        self.balance
            .to_unit_round_down_with_params(&self.gas_params.txn)
    }

    pub fn deduct_gas(&mut self, amount: InternalGas) -> PartialVMResult<()> {
        if !self.charge {
            return Ok(());
        }
        match self.balance.checked_sub(amount) {
            Some(new_balance) => {
                self.balance = new_balance;
                Ok(())
            }
            None => {
                self.balance = 0.into();
                Err(PartialVMError::new(StatusCode::OUT_OF_GAS))
            }
        }
    }

    pub fn set_metering(&mut self, enabled: bool) {
        self.charge = enabled;
    }

    pub fn charge_intrinsic_gas_for_transaction(&mut self, txn_size: NumBytes) -> VMResult<()> {
        let cost = self.gas_params.txn.calculate_intrinsic_gas(txn_size);
        info!("charge_intrinsic_gas cost InternalGasUnits({})", cost);
        self.deduct_gas(cost)
            .map_err(|e| e.finish(Location::Undefined))
    }

    pub fn cal_write_set_gas(&self) -> InternalGas {
        self.gas_params.txn.cal_write_set_gas()
    }
}

impl GasMeter for StarcoinGasMeter {
    #[inline]
    fn charge_simple_instr(&mut self, instr: SimpleInstruction) -> PartialVMResult<()> {
        let cost = self.gas_params.instr.simple_instr_cost(instr)?;
        info!(
            "charge_simple_instr instr {:#?} cost InternalGasUnits({})",
            instr, cost
        );
        self.deduct_gas(cost)
    }

    #[inline]
    fn charge_call(
        &mut self,
        _module_id: &ModuleId,
        _func_name: &str,
        args: impl ExactSizeIterator<Item = impl ValueView>,
    ) -> PartialVMResult<()> {
        let params = &self.gas_params.instr;

        let cost = params.call_per_arg * NumArgs::new(args.len() as u64 + 1);
        info!("charge_CALL cost InternalGasUnits({})", cost);

        self.deduct_gas(cost)
    }

    #[inline]
    fn charge_call_generic(
        &mut self,
        _module_id: &ModuleId,
        _func_name: &str,
        ty_args: impl ExactSizeIterator<Item = impl TypeView>,
        args: impl ExactSizeIterator<Item = impl ValueView>,
    ) -> PartialVMResult<()> {
        let params = &self.gas_params.instr;

        let cost =
            params.call_generic_per_arg * NumArgs::new((ty_args.len() + args.len() + 1) as u64);
        info!("charge_CALL_GENERIC cost InternalGasUnits({})", cost);
        self.deduct_gas(cost)
    }

    #[inline]
    fn charge_ld_const(&mut self, size: NumBytes) -> PartialVMResult<()> {
        let instr = &self.gas_params.instr;
        let cost = instr.ld_const_per_byte * size;
        info!("charge_LD_CONST cost InternalGasUnits({})", cost);
        self.deduct_gas(cost)
    }

    #[inline]
    fn charge_copy_loc(&mut self, val: impl ValueView) -> PartialVMResult<()> {
        let instr_params = &self.gas_params.instr;
        let cost = instr_params.copy_loc_per_abs_mem_unit * val.legacy_abstract_memory_size();
        info!("charge_COPY_LOC cost InternalGasUnits({})", cost);
        self.deduct_gas(cost)
    }

    #[inline]
    fn charge_move_loc(&mut self, val: impl ValueView) -> PartialVMResult<()> {
        let cost =
            self.gas_params.instr.move_loc_per_abs_mem_unit * val.legacy_abstract_memory_size();
        info!("charge_MOVE_LOC cost InternalGasUnits({})", cost);
        self.deduct_gas(cost)
    }

    #[inline]
    fn charge_store_loc(&mut self, val: impl ValueView) -> PartialVMResult<()> {
        let cost =
            self.gas_params.instr.st_loc_per_abs_mem_unit * val.legacy_abstract_memory_size();
        info!("charge_STORE_LOC cost InternalGasUnits({})", cost);
        self.deduct_gas(cost)
    }

    #[inline]
    fn charge_pack(
        &mut self,
        is_generic: bool,
        args: impl ExactSizeIterator<Item = impl ValueView>,
    ) -> PartialVMResult<()> {
        let field_count = AbstractMemorySize::new(args.len() as u64);
        let params = &self.gas_params.instr;
        let size = args.fold(field_count, |acc, val| {
            acc + val.legacy_abstract_memory_size()
        });
        let cost = match is_generic {
            false => params.pack_per_abs_mem_unit * size,
            true => params.pack_generic_per_abs_mem_unit * size,
        };
        if is_generic {
            info!("charge_PACK_GENERIC cost InternalGasUnits({})", cost);
        } else {
            info!("charge_PACK cost InternalGasUnits({})", cost);
        }
        self.deduct_gas(cost)
    }

    #[inline]
    fn charge_unpack(
        &mut self,
        is_generic: bool,
        args: impl ExactSizeIterator<Item = impl ValueView>,
    ) -> PartialVMResult<()> {
        let field_count = AbstractMemorySize::new(args.len() as u64);
        let params = &self.gas_params.instr;
        let size = args.fold(field_count, |acc, val| {
            acc + val.legacy_abstract_memory_size()
        });
        let cost = match is_generic {
            false => params.unpack_per_abs_mem_unit * size,
            true => params.unpack_generic_per_abs_mem_unit * size,
        };
        if is_generic {
            info!("charge_UNPACK_GENERIC cost InternalGasUnits({})", cost);
        } else {
            info!("charge_UNPACK cost InternalGasUnits({})", cost);
        }
        self.deduct_gas(cost)
    }

    #[inline]
    fn charge_read_ref(&mut self, val: impl ValueView) -> PartialVMResult<()> {
        let cost =
            self.gas_params.instr.read_ref_per_abs_mem_unit * val.legacy_abstract_memory_size();
        info!("charge_READ_REF cost InternalGasUnits({})", cost);
        self.deduct_gas(cost)
    }

    #[inline]
    fn charge_write_ref(&mut self, val: impl ValueView) -> PartialVMResult<()> {
        let cost =
            self.gas_params.instr.write_ref_per_abs_mem_unit * val.legacy_abstract_memory_size();
        info!("charge_WRITE_REF cost InternalGasUnits({})", cost);
        self.deduct_gas(cost)
    }

    #[inline]
    fn charge_eq(&mut self, lhs: impl ValueView, rhs: impl ValueView) -> PartialVMResult<()> {
        let instr_params = &self.gas_params.instr;
        let cost = instr_params.eq_per_abs_mem_unit
            * (lhs.legacy_abstract_memory_size() + rhs.legacy_abstract_memory_size());
        info!("charge_EQ cost InternalGasUnits({})", cost);
        self.deduct_gas(cost)
    }

    #[inline]
    fn charge_neq(&mut self, lhs: impl ValueView, rhs: impl ValueView) -> PartialVMResult<()> {
        let instr_params = &self.gas_params.instr;
        let cost = instr_params.eq_per_abs_mem_unit
            * (lhs.legacy_abstract_memory_size() + rhs.legacy_abstract_memory_size());
        info!("charge_NEQ cost InternalGasUnits({})", cost);
        self.deduct_gas(cost)
    }

    #[inline]
    fn charge_borrow_global(
        &mut self,
        is_mut: bool,
        is_generic: bool,
        _ty: impl TypeView,
        _is_success: bool,
    ) -> PartialVMResult<()> {
        let params = &self.gas_params.instr;
        let cost = match (is_mut, is_generic) {
            (false, false) => params.imm_borrow_global_base,
            (false, true) => params.imm_borrow_global_generic_base,
            (true, false) => params.mut_borrow_global_base,
            (true, true) => params.mut_borrow_global_generic_base,
        };
        info!(
            "charge_BORROW_GLOBAL {} {} InternalGasUnits({})",
            is_mut, is_generic, cost
        );
        self.deduct_gas(cost)
    }

    #[inline]
    fn charge_exists(
        &mut self,
        is_generic: bool,
        _ty: impl TypeView,
        exists: bool,
    ) -> PartialVMResult<()> {
        // XXX FIXME YSG, confirm it
        let reference_size: AbstractMemorySize = AbstractMemorySize::new(8);
        let min_exists_data_size: AbstractMemorySize = AbstractMemorySize::new(100);
        let params = &self.gas_params.instr;
        let param = match is_generic {
            false => params.exists_per_abs_mem_unit,
            true => params.exists_generic_per_abs_mem_unit,
        };
        let size = match exists {
            false => min_exists_data_size,
            true => reference_size,
        };
        let cost = param * size;
        info!(
            "charge_EXISTS {} cost InternalGasUnits({})",
            is_generic, cost
        );
        self.deduct_gas(cost)
    }

    #[inline]
    fn charge_move_from(
        &mut self,
        is_generic: bool,
        _ty: impl TypeView,
        val: Option<impl ValueView>,
    ) -> PartialVMResult<()> {
        if let Some(val) = val {
            let params = &self.gas_params.instr;
            let param = match is_generic {
                false => params.move_from_per_abs_mem_unit,
                true => params.move_from_generic_per_abs_mem_unit,
            };
            let cost = param * val.legacy_abstract_memory_size();
            info!(
                "charge_MOVE_FROM {} cost InternalGasUnits({})",
                is_generic, cost
            );
            return self.deduct_gas(cost);
        }
        Ok(())
    }

    #[inline]
    fn charge_move_to(
        &mut self,
        is_generic: bool,
        _ty: impl TypeView,
        val: impl ValueView,
        _is_success: bool,
    ) -> PartialVMResult<()> {
        let params = &self.gas_params.instr;
        let param = match is_generic {
            false => params.move_to_per_abs_mem_unit,
            true => params.move_to_generic_per_abs_mem_unit,
        };
        let cost = param * val.legacy_abstract_memory_size();
        info!(
            "charge_MOVE_TO {} cost InternalGasUnits({})",
            is_generic, cost
        );
        self.deduct_gas(cost)
    }

    #[inline]
    fn charge_vec_pack<'a>(
        &mut self,
        _ty: impl TypeView + 'a,
        args: impl ExactSizeIterator<Item = impl ValueView>,
    ) -> PartialVMResult<()> {
        let num_args = NumArgs::new(args.len() as u64);
        let params = &self.gas_params.instr;
        let cost = params.vec_pack_per_elem * num_args;
        info!("charge_VEC_PACK cost InternalGasUnits({})", cost);
        self.deduct_gas(cost)
    }

    #[inline]
    fn charge_vec_len(&mut self, _ty: impl TypeView) -> PartialVMResult<()> {
        let cost = self.gas_params.instr.vec_len_base;
        info!("charge_VEC_LEN cost InternalGasUnits({})", cost);
        self.deduct_gas(self.gas_params.instr.vec_len_base)
    }

    #[inline]
    fn charge_vec_borrow(
        &mut self,
        is_mut: bool,
        _ty: impl TypeView,
        _is_success: bool,
    ) -> PartialVMResult<()> {
        let params = &self.gas_params.instr;
        let cost = match is_mut {
            false => params.vec_imm_borrow_base,
            true => params.vec_mut_borrow_base,
        };
        info!(
            "charge_VEC_BORROW {} cost InternalGasUnits({})",
            is_mut, cost
        );
        self.deduct_gas(cost)
    }

    #[inline]
    fn charge_vec_push_back(
        &mut self,
        _ty: impl TypeView,
        val: impl ValueView,
    ) -> PartialVMResult<()> {
        let cost = self.gas_params.instr.vec_push_back_per_abs_mem_unit
            * val.legacy_abstract_memory_size();
        info!("charge_VEC_PUSH_BACK cost InternalGasUnits({})", cost);
        self.deduct_gas(cost)
    }

    #[inline]
    fn charge_vec_pop_back(
        &mut self,
        _ty: impl TypeView,
        _val: Option<impl ValueView>,
    ) -> PartialVMResult<()> {
        let cost = self.gas_params.instr.vec_pop_back_base;
        info!("charge_VEC_POP_BACK cost InternalGasUnits({})", cost);
        self.deduct_gas(cost)
    }

    #[inline]
    fn charge_vec_unpack(
        &mut self,
        _ty: impl TypeView,
        expect_num_elements: NumArgs,
    ) -> PartialVMResult<()> {
        let cost = self.gas_params.instr.vec_unpack_per_expected_elem * expect_num_elements;
        info!("charge_VEC_UNPACK cost InternalGasUnits({})", cost);
        self.deduct_gas(cost)
    }

    #[inline]
    fn charge_vec_swap(&mut self, _ty: impl TypeView) -> PartialVMResult<()> {
        let cost = self.gas_params.instr.vec_swap_base;
        info!("charge_VEC_SWAP cost InternalGasUnits({})", cost);
        self.deduct_gas(cost)
    }

    #[inline]
    fn charge_load_resource(&mut self, _loaded: Option<NumBytes>) -> PartialVMResult<()> {
        info!("charge_load_resource cost");
        Ok(())
    }

    #[inline]
    fn charge_native_function(&mut self, amount: InternalGas) -> PartialVMResult<()> {
        info!("charge_NATIVE_FUNCTION cost InternalGasUnits({})", amount);
        self.deduct_gas(amount)
    }
}