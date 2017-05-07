// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

use fmt;
use tendril::{Tendril, Atomicity};
use utf8;

pub enum Utf8DecodeError<A> where A: Atomicity {
    Invalid {
        valid_prefix: Tendril<fmt::UTF8, A>,
        remaining_input: Tendril<fmt::Bytes, A>,
    },
    Incomplete {
        valid_prefix: Tendril<fmt::UTF8, A>,
        incomplete_suffix: IncompleteUtf8,
    },
}

pub struct IncompleteUtf8(utf8::Incomplete);

impl<A> Tendril<fmt::Bytes, A> where A: Atomicity {
    pub fn decode_utf8(mut self) -> Result<Tendril<fmt::UTF8, A>, Utf8DecodeError<A>> {
        let unborrowed_result = match utf8::decode(&self) {
            Ok(s) => {
                debug_assert!(s.as_ptr() == self.as_ptr());
                debug_assert!(s.len() == self.len());
                Ok(())
            }
            Err(utf8::DecodeError::Invalid { valid_prefix, invalid_sequence, .. }) => {
                debug_assert!(valid_prefix.as_ptr() == self.as_ptr());
                debug_assert!(valid_prefix.len() <= self.len());
                Err((valid_prefix.len(), Err(valid_prefix.len() + invalid_sequence.len())))
            }
            Err(utf8::DecodeError::Incomplete { valid_prefix, incomplete_suffix }) => {
                debug_assert!(valid_prefix.as_ptr() == self.as_ptr());
                debug_assert!(valid_prefix.len() <= self.len());
                Err((valid_prefix.len(), Ok(incomplete_suffix)))
            }
        };
        match unborrowed_result {
            Ok(()) => {
                unsafe {
                    Ok(self.reinterpret_without_validating())
                }
            }
            Err((valid_len, and_then)) => {
                let subtendril = self.subtendril(0, valid_len as u32);
                let valid = unsafe {
                    subtendril.reinterpret_without_validating()
                };
                match and_then {
                    Ok(incomplete) => Err(Utf8DecodeError::Incomplete {
                        valid_prefix: valid,
                        incomplete_suffix: IncompleteUtf8(incomplete),
                    }),
                    Err(offset) => Err(Utf8DecodeError::Invalid {
                        valid_prefix: valid,
                        remaining_input: {
                            self.pop_front(offset as u32);
                            self
                        },
                    }),
                }
            }
        }
    }
}

impl IncompleteUtf8 {
    pub fn try_complete<A>(&mut self, mut input: Tendril<fmt::Bytes, A>)
                           -> Option<(Result<Tendril<fmt::UTF8, A>, ()>, Tendril<fmt::Bytes, A>)>
    where A: Atomicity {
        let result;
        let resume_at;
        match self.0.try_complete(&input) {
            None => return None,
            Some((res, rest)) => {
                result = res.map(Tendril::from_slice).map_err(|_| ());
                resume_at = input.len() - rest.len();
            }
        }
        input.pop_front(resume_at as u32);
        Some((result, input))
    }
}
