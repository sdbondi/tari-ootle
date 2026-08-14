//    Copyright 2024 The Tari Project
//    SPDX-License-Identifier: BSD-3-Clause

use std::marker::PhantomData;

use tari_ootle_transaction::Transaction;

use crate::Validator;

/// No Op validator - does nothing. Generic on any context or error
#[derive(Debug)]
pub struct NoopValidator<Ctx, Err>(PhantomData<(Ctx, Err)>);

impl<Ctx, Err> NoopValidator<Ctx, Err> {
    pub const fn new() -> Self {
        Self(PhantomData)
    }
}

impl<Ctx, Err> Validator<Transaction> for NoopValidator<Ctx, Err> {
    type Context = Ctx;
    type Error = Err;

    fn validate(&self, _context: &Ctx, _transaction: &Transaction) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl<Ctx, Err> Clone for NoopValidator<Ctx, Err> {
    fn clone(&self) -> Self {
        NoopValidator(PhantomData)
    }
}

impl<Ctx, Err> Default for NoopValidator<Ctx, Err> {
    fn default() -> Self {
        NoopValidator::new()
    }
}
