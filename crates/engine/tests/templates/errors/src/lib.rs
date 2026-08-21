//   Copyright 2022. The Tari Project
//
//   Redistribution and use in source and binary forms, with or without modification, are permitted provided that the
//   following conditions are met:
//
//   1. Redistributions of source code must retain the above copyright notice, this list of conditions and the following
//   disclaimer.
//
//   2. Redistributions in binary form must reproduce the above copyright notice, this list of conditions and the
//   following disclaimer in the documentation and/or other materials provided with the distribution.
//
//   3. Neither the name of the copyright holder nor the names of its contributors may be used to endorse or promote
//   products derived from this software without specific prior written permission.
//
//   THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS" AND ANY EXPRESS OR IMPLIED WARRANTIES,
//   INCLUDING, BUT NOT LIMITED TO, THE IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE ARE
//   DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT HOLDER OR CONTRIBUTORS BE LIABLE FOR ANY DIRECT, INDIRECT, INCIDENTAL,
//   SPECIAL, EXEMPLARY, OR CONSEQUENTIAL DAMAGES (INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR
//   SERVICES; LOSS OF USE, DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER CAUSED AND ON ANY THEORY OF LIABILITY,
//   WHETHER IN CONTRACT, STRICT LIABILITY, OR TORT (INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE
//   USE OF THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.
use tari_template_lib::prelude::*;

#[template]
mod template {
    use super::*;

    pub struct Errors;

    impl Errors {
        pub fn panic() {
            panic!("This error message should be included in the execution result")
        }

        pub fn please_pass_invalid_args(amount: Amount) {
            panic!("You didn't pass an invalid arg! {}", amount);
        }

        pub fn invalid_engine_call() {
            let resource_addr = ResourceAddress::new([123u8; 32].into());
            // Cannot create a vault for a resource that doesnt exist
            let vault = Vault::new_empty(resource_addr);
        }

        /// Calls the engine with an op no `EngineOp` maps to, and carries on as if it had worked.
        /// The entrypoint can only answer with a null pointer, and this ignores it: the refusal has
        /// to fail the transaction on the engine's side alone.
        pub fn ignore_refused_engine_call() {
            let arg = [0u8; 1];
            unsafe {
                tari_template_abi::tari_engine(i32::MAX, arg.as_ptr(), arg.len());
            }
        }

        /// As above, but with a valid op and an argument the engine cannot decode.
        pub fn ignore_undecodable_engine_call() {
            // 0xff is a CBOR break byte, which is not a value.
            let arg = [0xffu8; 8];
            unsafe {
                tari_template_abi::tari_engine(
                    tari_template_abi::EngineOp::EmitLog.as_i32(),
                    arg.as_ptr(),
                    arg.len(),
                );
            }
        }
    }
}
